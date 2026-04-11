use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use super::types::{extract_option_inner, extract_result_types};
use crate::util::panic_path;

/// Generate code to decode a return value from `retbuffer[__off..len]`,
/// handling `Option<T>` and `Result<T, E>` with tag+payload decoding.
///
/// Returns a `TokenStream` that evaluates to the decoded value.
pub fn gen_decode_return_value(rt: &syn::Type, server_expr: &TokenStream2) -> TokenStream2 {
    let _p = panic_path();
    if let Some(inner) = extract_option_inner(rt) {
        quote! {{
            if __off >= len {
                #_p!("ipc: server {} sent truncated reply", #server_expr);
            }
            match retbuffer[__off] {
                0u8 => {
                    __off += 1;
                    let Some((__val, _)) = ipc::wire::read::<#inner>(&retbuffer[__off..len]) else {
                        #_p!("ipc: server {} sent malformed reply ({} bytes)", #server_expr, len);
                    };
                    __off += core::mem::size_of_val(&__val);
                    Some(__val)
                }
                1u8 => {
                    __off += 1;
                    None
                }
                __tag => #_p!("ipc: server {} sent invalid option tag {}", #server_expr, __tag),
            }
        }}
    } else if let Some((ok_ty, err_ty)) = extract_result_types(rt) {
        let is_unit = matches!(ok_ty, syn::Type::Tuple(t) if t.elems.is_empty());
        let read_ok = if is_unit {
            quote! { () }
        } else {
            quote! {{
                let Some((__val, _)) = ipc::wire::read::<#ok_ty>(&retbuffer[__off..len]) else {
                    #_p!("ipc: server {} sent malformed reply ({} bytes)", #server_expr, len);
                };
                __off += core::mem::size_of_val(&__val);
                __val
            }}
        };
        quote! {{
            if __off >= len {
                #_p!("ipc: server {} sent truncated reply", #server_expr);
            }
            match retbuffer[__off] {
                0u8 => {
                    __off += 1;
                    Ok(#read_ok)
                }
                1u8 => {
                    __off += 1;
                    let Some((__err, _)) = ipc::wire::read::<#err_ty>(&retbuffer[__off..len]) else {
                        #_p!("ipc: server {} sent malformed error reply ({} bytes)", #server_expr, len);
                    };
                    __off += core::mem::size_of_val(&__err);
                    Err(__err)
                }
                __tag => #_p!("ipc: server {} sent invalid result tag {}", #server_expr, __tag),
            }
        }}
    } else {
        quote! {{
            let Some((__val, _)) = ipc::wire::read::<#rt>(&retbuffer[__off..len]) else {
                #_p!("ipc: server {} sent malformed reply ({} bytes)", #server_expr, len);
            };
            __off += core::mem::size_of_val(&__val);
            __val
        }}
    }
}
