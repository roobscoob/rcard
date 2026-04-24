use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use super::types::wire_type_for;
use crate::parse::ParsedParam;

/// Deserialize method arguments via postcard.
///
/// Emits a single `postcard::from_bytes` call that decodes a tuple of
/// `(handle?, args...)` — matching the shape serialized by
/// `gen_serialize_wire`. The tuple is then destructured into named
/// bindings. Byte layout is defined by postcard's serde tuple impl,
/// not by memory layout — target-agnostic.
///
/// Returns `(deserialize_stmts, destructure_stmts)`. Unlike the old
/// zerocopy path which threaded `__buf` through per-field reads, the
/// destructure happens inline in a single `let` binding, so the
/// second return is empty.
pub fn gen_deserialize_args(
    non_lease_params: &[&ParsedParam],
    include_handle: bool,
) -> (TokenStream2, TokenStream2) {
    // Early-out: no args at all (no handle, no params) — nothing to decode.
    if !include_handle && non_lease_params.is_empty() {
        return (
            quote! {
                let _ = msg.raw_data();
            },
            quote! {},
        );
    }

    // Build the tuple type annotation: (HandleTy?, ArgTy1, ArgTy2, ...).
    let mut tuple_types: Vec<TokenStream2> = Vec::new();
    if include_handle {
        tuple_types.push(quote! { ipc::RawHandle });
    }
    for p in non_lease_params {
        tuple_types.push(wire_type_for(p));
    }

    // Matching bindings: (handle_bind?, pname1, pname2, ...).
    let mut binding_idents: Vec<TokenStream2> = Vec::new();
    if include_handle {
        binding_idents.push(quote! { handle });
    }
    for p in non_lease_params {
        let n = &p.name;
        binding_idents.push(quote! { #n });
    }

    let deserialize = quote! {
        let __buf = msg.raw_data();
        let ( #( #binding_idents ,)* ): ( #( #tuple_types ,)* ) =
            match ipc::__postcard::from_bytes(__buf) {
                Ok(t) => t,
                Err(_) => {
                    // Diagnostic payload: [buf_len, first up to 16 bytes]
                    // Lets the host tell "no bytes" vs "wrong bytes"
                    // vs "too many bytes" apart at a glance.
                    let mut __diag = [0u8; 17];
                    __diag[0] = __buf.len() as u8;
                    let __copy_len = __buf.len().min(16);
                    __diag[1..1 + __copy_len].copy_from_slice(&__buf[..__copy_len]);
                    reply.reply_error(ipc::MALFORMED_MESSAGE, &__diag[..1 + __copy_len]);
                    return;
                }
            };
    };

    (deserialize, quote! {})
}
