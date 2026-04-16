use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

/// Generate code that encodes a return value into `__reply_buf` at
/// offset `__off`, via postcard.
///
/// The caller supplies a `__reply_buf: [MaybeUninit<u8>; N]` and a
/// mutable `__off: usize`. We cast the uninit tail to `&mut [u8]` for
/// postcard to write into — sound because postcard only *writes* to
/// the buffer, never reads uninitialized bytes.
///
/// No custom Option/Result tag handling: postcard serializes those
/// natively via serde's variant-index encoding. Both sides of the
/// wire change together, so the tag-byte mismatch versus the old
/// hand-rolled encoding is internally consistent.
pub fn gen_encode_return_value(_rt: &syn::Type, value_expr: TokenStream2) -> TokenStream2 {
    quote! {
        {
            // SAFETY: postcard::to_slice only writes to the buffer.
            // `MaybeUninit<u8>` has the same layout as `u8`, and we
            // only treat the written prefix as initialized afterward
            // via the caller's `assume_init_slice` call.
            let __tail: &mut [u8] = unsafe {
                core::slice::from_raw_parts_mut(
                    __reply_buf.as_mut_ptr().add(__off) as *mut u8,
                    __reply_buf.len() - __off,
                )
            };
            match ipc::__postcard::to_slice(&(#value_expr), __tail) {
                Ok(slice) => {
                    __off += slice.len();
                }
                Err(_) => {
                    ipc::__ipc_panic!("postcard reply encode failed");
                }
            }
        }
    }
}
