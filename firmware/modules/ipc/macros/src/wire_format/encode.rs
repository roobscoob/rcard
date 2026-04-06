use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use super::types::{extract_option_inner, extract_result_types};

/// Generate code to write a return value into `__reply_buf` at offset `__off`,
/// handling `Option<T>` and `Result<T, E>` with tag+payload encoding.
///
/// `__reply_buf` is `[MaybeUninit<u8>; N]`. The generated code writes bytes and
/// updates `__off`.
pub fn gen_encode_return_value(rt: &syn::Type, value_expr: TokenStream2) -> TokenStream2 {
    if extract_option_inner(rt).is_some() {
        quote! {
            match #value_expr {
                Some(ref __val) => {
                    ipc::wire::set_uninit(&mut __reply_buf, __off, 0u8); // Some
                    __off += 1;
                    __off += ipc::wire::write_uninit(&mut __reply_buf[__off..], __val);
                }
                None => {
                    ipc::wire::set_uninit(&mut __reply_buf, __off, 1u8); // None
                    __off += 1;
                }
            }
        }
    } else if let Some((ok_ty, _err_ty)) = extract_result_types(rt) {
        // Check if Ok type is () — don't write anything for unit
        let is_unit = matches!(ok_ty, syn::Type::Tuple(t) if t.elems.is_empty());
        let write_ok = if is_unit {
            quote! {}
        } else {
            quote! { __off += ipc::wire::write_uninit(&mut __reply_buf[__off..], __ok_val); }
        };
        quote! {
            match #value_expr {
                Ok(ref __ok_val) => {
                    ipc::wire::set_uninit(&mut __reply_buf, __off, 0u8); // Ok
                    __off += 1;
                    #write_ok
                }
                Err(ref __err_val) => {
                    ipc::wire::set_uninit(&mut __reply_buf, __off, 1u8); // Err
                    __off += 1;
                    __off += ipc::wire::write_uninit(&mut __reply_buf[__off..], __err_val);
                }
            }
        }
    } else {
        quote! {
            __off += ipc::wire::write_uninit(&mut __reply_buf[__off..], &#value_expr);
        }
    }
}
