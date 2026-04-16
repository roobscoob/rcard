use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::util::panic_path;

/// Generate code that decodes a return value from `retbuffer[__off..len]`
/// via postcard, updating `__off` to point past the consumed bytes.
///
/// No custom Option/Result tag handling: postcard decodes those
/// natively via serde.
pub fn gen_decode_return_value(rt: &syn::Type, server_expr: &TokenStream2) -> TokenStream2 {
    let _p = panic_path();
    quote! {{
        let __in = &retbuffer[__off..len];
        match ipc::__postcard::take_from_bytes::<#rt>(__in) {
            Ok((__val, __rest)) => {
                __off += __in.len() - __rest.len();
                __val
            }
            Err(_) => {
                #_p!(
                    "ipc: server {} sent malformed reply ({} bytes)",
                    #server_expr, len
                );
            }
        }
    }}
}
