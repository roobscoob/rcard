use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::lease;
use crate::parse::{HandleMode, MethodKind, ParsedMethod, ParsedParam};
use crate::transfer;
use crate::util::{replace_ident_in_type, to_pascal_case};
use crate::wire_format;

use super::constructs::gen_constructs_map;
use super::error_types::error_type_for;
use super::reply::{gen_ctor_reply, gen_parse_reply};

/// Generate the method implementation for a single method on a concrete client.
pub fn gen_client_method(m: &ParsedMethod, kind: u8, enum_name: &Ident) -> TokenStream2 {
    let method_name = &m.name;
    let variant = format_ident!("{}", to_pascal_case(&method_name.to_string()));
    let method_id_expr = quote! { #enum_name::#variant as u8 };
    let err_type = error_type_for(m.kind, &m.params);

    let non_lease_params: Vec<&ParsedParam> = m.params.iter().filter(|p| !p.is_lease).collect();
    let lease_params: Vec<&ParsedParam> = m.params.iter().filter(|p| p.is_lease).collect();

    let sig_params: Vec<TokenStream2> = m
        .params
        .iter()
        .map(|p| {
            let pname = &p.name;
            match p.handle_mode {
                Some(HandleMode::Move) => quote! { #pname: impl ipc::Transferable },
                Some(HandleMode::Clone) => quote! { #pname: &impl ipc::Cloneable },
                None => {
                    let ty = &p.ty;
                    quote! { #pname: #ty }
                }
            }
        })
        .collect();

    match m.kind {
        MethodKind::Constructor => {
            let ctor_return = m
                .ctor_return
                .as_ref()
                .expect("constructor must have ctor_return");
            gen_constructor(
                method_name,
                &method_id_expr,
                kind,
                &sig_params,
                &non_lease_params,
                &lease_params,
                ctor_return,
                &err_type,
            )
        }
        MethodKind::Message => gen_message(
            method_name,
            &method_id_expr,
            kind,
            &sig_params,
            &non_lease_params,
            &lease_params,
            m.return_type.as_ref(),
            m.constructs.as_ref(),
            &err_type,
        ),
        MethodKind::StaticMessage => gen_static_message(
            method_name,
            &method_id_expr,
            kind,
            &sig_params,
            &non_lease_params,
            &lease_params,
            m.return_type.as_ref(),
            &err_type,
        ),
        MethodKind::Destructor => gen_destructor(
            method_name,
            &method_id_expr,
            kind,
            &sig_params,
            &non_lease_params,
            &lease_params,
            m.return_type.as_ref(),
            &err_type,
        ),
    }
}

fn gen_constructor(
    method_name: &Ident,
    method_id_expr: &TokenStream2,
    kind: u8,
    sig_params: &[TokenStream2],
    non_lease_params: &[&ParsedParam],
    lease_params: &[&ParsedParam],
    ctor_return: &crate::parse::ConstructorReturn,
    err_type: &TokenStream2,
) -> TokenStream2 {
    let ctor_server_expr = quote! { server.get() };
    let handle_transfer_stmts =
        transfer::gen_handle_transfer_stmts(non_lease_params, &ctor_server_expr, err_type);

    let wire_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let wire_types: Vec<TokenStream2> = non_lease_params
        .iter()
        .map(|p| wire_format::wire_type_for(p))
        .collect();

    let serialize = wire_format::gen_serialize_wire(&wire_names, &wire_types, None);
    let lease_arr = lease::gen_lease_array(lease_params);
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);

    let make_self = quote! {
        Self {
            server,
            handle: core::cell::Cell::new(handle),
            destroyed: core::cell::Cell::new(false),
            _server: core::marker::PhantomData,
        }
    };

    let (fn_ret, parse_and_map) = gen_ctor_reply(ctor_return, err_type, &make_self);

    quote! {
        pub fn #method_name(
            #(#sig_params),*
        ) -> #fn_ret {
            let server_id = S::server_id();
            let server = core::cell::Cell::new(server_id.get());
            #handle_transfer_stmts
            #serialize
            #lease_arr
            let opcode = ipc::opcode(#kind_lit, #method_id_expr);
            let len = ipc::call_send_unified(
                server_id,
                server.get(),
                opcode,
                n,
                &mut leases,
            ).map_err(#err_type::from_wire)?;
            let retbuffer: &[u8] = unsafe { &*ipc::ipc_buf() };
            #parse_and_map
        }
    }
}

fn gen_message(
    method_name: &Ident,
    method_id_expr: &TokenStream2,
    kind: u8,
    sig_params: &[TokenStream2],
    non_lease_params: &[&ParsedParam],
    lease_params: &[&ParsedParam],
    return_type: Option<&syn::Type>,
    constructs: Option<&(Ident, Ident)>,
    err_type: &TokenStream2,
) -> TokenStream2 {
    let self_server_expr = quote! { self.server.get() };
    let handle_transfer_stmts =
        transfer::gen_handle_transfer_stmts(non_lease_params, &self_server_expr, err_type);

    let wire_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let wire_types: Vec<TokenStream2> = non_lease_params
        .iter()
        .map(|p| wire_format::wire_type_for(p))
        .collect();

    let handle_expr = quote! { self.handle.get() };
    let serialize = wire_format::gen_serialize_wire(&wire_names, &wire_types, Some(&handle_expr));
    let lease_arr = lease::gen_lease_array(lease_params);
    let call_body = gen_instance_call_body(kind, method_id_expr, &lease_arr, err_type);

    if let Some((trait_name, generic_ident)) = constructs {
        let handle_type = format_ident!("{}Handle", trait_name);
        let server_trait = format_ident!("{}Server", trait_name);

        let (wire_rt, user_rt) = match return_type {
            Some(rt) => {
                let wire = replace_ident_in_type(rt, generic_ident, &quote! { ipc::RawHandle });
                let user = replace_ident_in_type(
                    rt,
                    generic_ident,
                    &quote! { #handle_type<#generic_ident> },
                );
                (quote! { #wire }, quote! { #user })
            }
            None => (quote! { () }, quote! { () }),
        };

        let parse_reply = gen_parse_reply(
            Some(&syn::parse2(wire_rt.clone()).unwrap()),
            quote! { self.server.get() },
        );
        let map_handles = gen_constructs_map(return_type, generic_ident, &handle_type);

        quote! {
            pub fn #method_name<#generic_ident: #server_trait>(&self, #(#sig_params),*)
                -> core::result::Result<#user_rt, #err_type>
            {
                #handle_transfer_stmts
                #serialize
                #call_body
                let wire_result: core::result::Result<_, ipc::Error> = { #parse_reply };
                wire_result.map(|v| { #map_handles }).map_err(#err_type::from_wire)
            }
        }
    } else {
        let parse_reply = gen_parse_reply(return_type, quote! { self.server.get() });

        let ret_type = match return_type {
            Some(rt) => quote! { -> core::result::Result<#rt, #err_type> },
            None => quote! { -> core::result::Result<(), #err_type> },
        };

        quote! {
            pub fn #method_name(&self, #(#sig_params),*) #ret_type {
                #handle_transfer_stmts
                #serialize
                #call_body
                let wire_result: core::result::Result<_, ipc::Error> = { #parse_reply };
                wire_result.map_err(#err_type::from_wire)
            }
        }
    }
}

fn gen_destructor(
    method_name: &Ident,
    method_id_expr: &TokenStream2,
    kind: u8,
    sig_params: &[TokenStream2],
    non_lease_params: &[&ParsedParam],
    lease_params: &[&ParsedParam],
    return_type: Option<&syn::Type>,
    err_type: &TokenStream2,
) -> TokenStream2 {
    let self_server_expr = quote! { self.server.get() };
    let handle_transfer_stmts =
        transfer::gen_handle_transfer_stmts(non_lease_params, &self_server_expr, err_type);

    let wire_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let wire_types: Vec<TokenStream2> = non_lease_params
        .iter()
        .map(|p| wire_format::wire_type_for(p))
        .collect();

    let handle_expr = quote! { self.handle.get() };
    let serialize = wire_format::gen_serialize_wire(&wire_names, &wire_types, Some(&handle_expr));
    let lease_arr = lease::gen_lease_array(lease_params);
    let call_body = gen_instance_call_body(kind, method_id_expr, &lease_arr, err_type);
    let parse_reply = gen_parse_reply(return_type, quote! { self.server.get() });

    let ret_type = match return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, #err_type> },
        None => quote! { -> core::result::Result<(), #err_type> },
    };

    quote! {
        pub fn #method_name(self, #(#sig_params),*) #ret_type {
            self.destroyed.set(true);
            #handle_transfer_stmts
            #serialize
            #call_body
            let wire_result: core::result::Result<_, ipc::Error> = { #parse_reply };
            wire_result.map_err(#err_type::from_wire)
        }
    }
}

fn gen_static_message(
    method_name: &Ident,
    method_id_expr: &TokenStream2,
    kind: u8,
    sig_params: &[TokenStream2],
    non_lease_params: &[&ParsedParam],
    lease_params: &[&ParsedParam],
    return_type: Option<&syn::Type>,
    err_type: &TokenStream2,
) -> TokenStream2 {
    let server_expr = quote! { server_id.get() };
    let handle_transfer_stmts =
        transfer::gen_handle_transfer_stmts(non_lease_params, &server_expr, err_type);

    let wire_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let wire_types: Vec<TokenStream2> = non_lease_params
        .iter()
        .map(|p| wire_format::wire_type_for(p))
        .collect();

    let serialize = wire_format::gen_serialize_wire(&wire_names, &wire_types, None);
    let lease_arr = lease::gen_lease_array(lease_params);
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);
    let parse_reply = gen_parse_reply(return_type, quote! { server_id.get() });

    let ret_type = match return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, #err_type> },
        None => quote! { -> core::result::Result<(), #err_type> },
    };

    quote! {
        pub fn #method_name(#(#sig_params),*) #ret_type {
            let server_id = S::server_id();
            #handle_transfer_stmts
            #serialize
            #lease_arr
            let opcode = ipc::opcode(#kind_lit, #method_id_expr);
            let len = ipc::call_send_unified(
                server_id,
                server_id.get(),
                opcode,
                n,
                &mut leases,
            ).map_err(#err_type::from_wire)?;
            let retbuffer: &[u8] = unsafe { &*ipc::ipc_buf() };
            let wire_result: core::result::Result<_, ipc::Error> = { #parse_reply };
            wire_result.map_err(#err_type::from_wire)
        }
    }
}

/// Build the post-serialize portion of instance-method bodies (`gen_message`
/// and `gen_destructor`). Emits `lease_arr` + opcode + retbuf alloc +
/// `call_send` invocation. On `ipc::Error::ServerDied` the caller's
/// mapping converts it to `HandleLost` (an instance-scoped death means the
/// handle key is gone); this mapping is uniform across all instance methods.
///
/// Produces a `let len: usize = ...` binding the reply length.
fn gen_instance_call_body(
    kind: u8,
    method_id_expr: &TokenStream2,
    lease_arr: &TokenStream2,
    err_type: &TokenStream2,
) -> TokenStream2 {
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);
    quote! {
        #lease_arr
        let opcode = ipc::opcode(#kind_lit, #method_id_expr);
        let len = ipc::call_send_unified(
            S::server_id(),
            self.server.get(),
            opcode,
            n,
            &mut leases,
        ).map_err(|_| #err_type::from_wire(ipc::Error::HandleLost))?;
        let retbuffer: &[u8] = unsafe { &*ipc::ipc_buf() };
    }
}
