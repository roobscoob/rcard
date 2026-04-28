use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::Ident;

use crate::lease;
use crate::parse::{ParsedMethod, ParsedParam};
use crate::transfer;
use crate::wire_format;

use super::error_types::error_type_for;
use super::reply::gen_parse_reply;

/// Generate a method implementation for a dynamic (interface) client.
pub fn gen_dyn_method(m: &ParsedMethod) -> TokenStream2 {
    let method_name = &m.name;
    let method_id_lit = proc_macro2::Literal::u8_suffixed(m.method_id);
    let err_type = error_type_for(m.kind, &m.params);

    let non_lease_params: Vec<&ParsedParam> = m.params.iter().filter(|p| !p.is_lease).collect();
    let lease_params: Vec<&ParsedParam> = m.params.iter().filter(|p| p.is_lease).collect();

    let sig_params: Vec<TokenStream2> = m
        .params
        .iter()
        .map(|p| {
            let pname = &p.name;
            let ty = &p.ty;
            quote! { #pname: #ty }
        })
        .collect();

    let server_expr = quote! { self.server.get() };
    let handle_transfer_stmts =
        transfer::gen_handle_transfer_stmts(&non_lease_params, &server_expr, &err_type);

    let wire_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let wire_types: Vec<TokenStream2> = non_lease_params
        .iter()
        .map(|p| wire_format::wire_type_for(p))
        .collect();

    let handle_expr = quote! { self.handle.get() };
    let serialize = wire_format::gen_serialize_wire(&wire_names, &wire_types, Some(&handle_expr));
    let lease_arr = lease::gen_lease_array(&lease_params);

    let ret_type = match &m.return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, #err_type> },
        None => quote! { -> core::result::Result<(), #err_type> },
    };

    let parse_reply = gen_parse_reply(m.return_type.as_ref(), quote! { self.server.get() });

    quote! {
        pub fn #method_name(&self, #(#sig_params),*) #ret_type {
            #handle_transfer_stmts
            #serialize
            #lease_arr
            let opcode = ipc::opcode(self.kind, #method_id_lit);
            let buf = unsafe { &mut *ipc::ipc_buf() };
            let (rc, len) = ipc::kern::sys_send(
                self.server.get(),
                opcode,
                buf,
                n,
                &mut leases,
            ).map_err(|_| #err_type::from_wire(ipc::Error::ServerDied))?;
            let retbuffer: &[u8] = &buf[..];
            let wire_result: core::result::Result<_, ipc::Error> = { #parse_reply };
            wire_result.map_err(#err_type::from_wire)
        }
    }
}

/// Static methods on dynamic clients need a server binding — not yet supported.
pub fn gen_dyn_static_method(_m: &ParsedMethod) -> TokenStream2 {
    quote! {}
}
