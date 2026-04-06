use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::Ident;

/// Serialize method arguments into `argbuffer` using sequential zerocopy writes.
///
/// `handle_expr` — if `Some`, a `RawHandle` is written first (instance methods).
/// Pass `None` for constructors and static messages.
pub fn gen_serialize_wire(
    wire_names: &[&Ident],
    wire_types: &[TokenStream2],
    handle_expr: Option<&TokenStream2>,
) -> TokenStream2 {
    if handle_expr.is_none() && wire_types.is_empty() {
        return quote! {
            let argbuffer = [];
            let n = 0usize;
        };
    }

    let mut writes = Vec::new();
    writes.push(quote! {
        let mut argbuffer: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
            unsafe { core::mem::MaybeUninit::uninit().assume_init() };
        let mut n = 0usize;
    });

    if let Some(handle) = handle_expr {
        writes.push(quote! {
            n += ipc::wire::write_uninit(&mut argbuffer[n..], &#handle);
        });
    }

    for (name, _ty) in wire_names.iter().zip(wire_types.iter()) {
        writes.push(quote! {
            n += ipc::wire::write_uninit(&mut argbuffer[n..], &#name);
        });
    }

    quote! { #(#writes)* }
}
