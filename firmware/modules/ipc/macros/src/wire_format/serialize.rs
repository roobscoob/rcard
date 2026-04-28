use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::Ident;

/// Serialize method arguments into `argbuffer` via postcard.
///
/// Emits code that builds a tuple of `(handle?, args...)` and calls
/// `postcard::to_slice` on it. The tuple is serialized as a sequence
/// via serde's tuple impl — byte layout is defined by postcard, not
/// by memory layout, so it's target-agnostic (firmware and host agree
/// regardless of word size).
///
/// `handle_expr` — if `Some`, a handle is written first (instance methods).
/// Pass `None` for constructors and static messages.
pub fn gen_serialize_wire(
    wire_names: &[&Ident],
    wire_types: &[TokenStream2],
    handle_expr: Option<&TokenStream2>,
) -> TokenStream2 {
    let _ = wire_types;

    let has_handle = handle_expr.is_some();
    if !has_handle && wire_names.is_empty() {
        return quote! {
            let n = 0usize;
        };
    }

    let tuple_items: Vec<TokenStream2> = {
        let mut v = Vec::new();
        if let Some(h) = handle_expr {
            v.push(quote! { &(#h) });
        }
        for name in wire_names {
            v.push(quote! { &#name });
        }
        v
    };

    quote! {
        let __tuple = ( #( #tuple_items ,)* );
        let n = {
            let __ipc_buf: &mut [u8] = unsafe {
                core::slice::from_raw_parts_mut(
                    ipc::ipc_buf() as *mut u8,
                    ipc::HUBRIS_MESSAGE_SIZE_LIMIT,
                )
            };
            match ipc::__postcard::to_slice(&__tuple, __ipc_buf) {
                Ok(slice) => slice.len(),
                Err(_) => ipc::__ipc_panic!("postcard arg encode failed"),
            }
        };
    }
}
