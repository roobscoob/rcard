use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use super::types::wire_type_for;
use crate::parse::ParsedParam;

/// Generate sequential zerocopy reads for message arguments on the server side.
///
/// Emits a series of `ipc::wire::read::<T>(__buf)` calls, threading the
/// remaining buffer through each read.
///
/// Returns `(deserialize_stmts, destructure_stmts)`. The destructure half is
/// currently empty since variables are bound inline, but the pair is kept for
/// future flexibility.
pub fn gen_deserialize_args(
    non_lease_params: &[&ParsedParam],
    include_handle: bool,
) -> (TokenStream2, TokenStream2) {
    let mut reads = Vec::new();

    reads.push(quote! { let mut __buf = msg.raw_data(); });

    if include_handle {
        reads.push(quote! {
            let Some((handle, __rest)) = ipc::wire::read::<ipc::RawHandle>(__buf) else {
                reply.reply_error(ipc::MALFORMED_MESSAGE, &[]);
                return;
            };
            __buf = __rest;
        });
    }

    for p in non_lease_params {
        let pname = &p.name;
        let ty = wire_type_for(p);
        reads.push(quote! {
            let Some((#pname, __rest)) = ipc::wire::read::<#ty>(__buf) else {
                reply.reply_error(ipc::MALFORMED_MESSAGE, &[]);
                return;
            };
            __buf = __rest;
        });
    }

    (quote! { #(#reads)* }, quote! {})
}
