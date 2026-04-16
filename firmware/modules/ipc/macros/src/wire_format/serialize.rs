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
        // Zero args. Emit an empty MaybeUninit buffer to match the
        // shape expected by the downstream `assume_init_slice` call
        // site. No postcard encoding happens.
        return quote! {
            let argbuffer: [core::mem::MaybeUninit<u8>; 0] = [];
            let n = 0usize;
        };
    }

    // Build a tuple of references so we don't move the args, even if
    // they're non-Copy. Serde serializes `&T` transparently as `T`.
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

    // A 1-tuple is syntactically `(x,)` in Rust — the trailing comma
    // is meaningful. We always emit with trailing comma so 1-tuples
    // serialize as tuples rather than being unwrapped to `T`.
    //
    // We keep `argbuffer` as `[MaybeUninit<u8>; N]` to match the
    // shape the downstream `sys_send` call site expects (it calls
    // `assume_init_slice`). Postcard needs `&mut [u8]`, so we cast
    // the uninit slice to a mut u8 slice for write-only access.
    // SAFETY: postcard::to_slice only WRITES to the buffer — never
    // reads uninitialized bytes.
    quote! {
        let mut argbuffer: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
            unsafe { core::mem::MaybeUninit::uninit().assume_init() };
        let __tuple = ( #( #tuple_items ,)* );
        let n = {
            let __tail: &mut [u8] = unsafe {
                core::slice::from_raw_parts_mut(
                    argbuffer.as_mut_ptr() as *mut u8,
                    argbuffer.len(),
                )
            };
            match ipc::__postcard::to_slice(&__tuple, __tail) {
                Ok(slice) => slice.len(),
                Err(_) => ipc::__ipc_panic!("postcard arg encode failed"),
            }
        };
    }
}
