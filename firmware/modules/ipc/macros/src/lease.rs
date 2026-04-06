use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::parse::ParsedParam;

// ---------------------------------------------------------------------------
// Client side
// ---------------------------------------------------------------------------

/// Build the lease array passed to `sys_send` on the client side.
///
/// Read-only leases use `Lease::read_only`, mutable leases use
/// `Lease::read_write`.
pub fn gen_lease_array(lease_params: &[&ParsedParam]) -> TokenStream2 {
    if lease_params.is_empty() {
        quote! { let mut leases = []; }
    } else {
        let exprs: Vec<TokenStream2> = lease_params
            .iter()
            .map(|p| {
                let pname = &p.name;
                if p.lease_mutable {
                    quote! { ipc::kern::Lease::read_write(zerocopy::IntoBytes::as_mut_bytes(#pname)) }
                } else {
                    quote! { ipc::kern::Lease::read_only(zerocopy::IntoBytes::as_bytes(#pname)) }
                }
            })
            .collect();
        quote! { let mut leases = [#(#exprs),*]; }
    }
}

// ---------------------------------------------------------------------------
// Server side
// ---------------------------------------------------------------------------

/// Bind lease parameters from the incoming message on the server side.
///
/// Each lease is wrapped in a `LeaseBorrow<Read>` or `LeaseBorrow<Write>`
/// depending on the declared mutability.
pub fn gen_lease_bindings(lease_params: &[&ParsedParam]) -> Vec<TokenStream2> {
    lease_params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let pname = &p.name;
            if p.lease_mutable {
                quote! {
                    let #pname = match msg.lease::<ipc::dispatch::Write>(#i) {
                        Ok(v) => v,
                        Err(_) => { reply.reply_error(ipc::MALFORMED_MESSAGE, &[]); return; }
                    };
                }
            } else {
                quote! {
                    let #pname = match msg.lease::<ipc::dispatch::Read>(#i) {
                        Ok(v) => v,
                        Err(_) => { reply.reply_error(ipc::MALFORMED_MESSAGE, &[]); return; }
                    };
                }
            }
        })
        .collect()
}
