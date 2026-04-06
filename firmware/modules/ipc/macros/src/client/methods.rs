use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::lease;
use crate::parse::{HandleMode, MethodKind, ParsedMethod, ParsedParam};
use crate::transfer;
use crate::util::{panic_path, replace_ident_in_type, to_pascal_case};
use crate::wire_format;

use super::constructs::gen_constructs_map;
use super::error_types::error_type_for;
use super::reply::{ctor_retbuffer_size, gen_ctor_reply, gen_parse_reply};

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
    let _p = panic_path();
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

    let retbuf_size = ctor_retbuffer_size(ctor_return);
    let (fn_ret, parse_and_map) = gen_ctor_reply(ctor_return, err_type, &make_self);

    quote! {
        pub fn #method_name(
            #(#sig_params),*
        ) -> #fn_ret {
            let server_id = S::server_id();
            let server = core::cell::Cell::new(server_id.get());
            #handle_transfer_stmts
            #serialize
            let mut __retbuf_mem: [core::mem::MaybeUninit<u8>; #retbuf_size] =
                unsafe { core::mem::MaybeUninit::uninit().assume_init() };
            let retbuffer = unsafe { ipc::wire::as_mut_byte_slice(&mut __retbuf_mem) };
            #lease_arr
            let argbuffer = unsafe { ipc::wire::assume_init_slice(&argbuffer, n) };
            let opcode = ipc::opcode(#kind_lit, #method_id_expr);
            let (rc, len) = ipc::kern::sys_send(
                server.get(),
                opcode,
                argbuffer,
                retbuffer,
                &mut leases,
            ).map_err(|dead| {
                server_id.set(server.get().with_generation(dead.new_generation()));
                #err_type::from_wire(ipc::Error::ServerDied)
            })?;
            if rc == ipc::ACCESS_VIOLATION {
                #_p!(
                    "ipc: server {:?} rejected our message: access violation \
                     (this task is not authorized to use this server)",
                    server.get(),
                );
            }
            if rc != ipc::kern::ResponseCode::SUCCESS {
                #_p!(
                    "ipc: server {:?} sent unexpected non-SUCCESS response code",
                    server.get(),
                );
            }
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
    let send_body = gen_send_body(kind, method_id_expr, &lease_arr);

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
                #send_body
                let (rc, len) = send_result.map_err(|dead| {
                    S::server_id().set(self.server.get().with_generation(dead.new_generation()));
                    #err_type::from_wire(ipc::Error::HandleLost)
                })?;
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
                #send_body
                let (rc, len) = send_result.map_err(|dead| {
                    S::server_id().set(self.server.get().with_generation(dead.new_generation()));
                    #err_type::from_wire(ipc::Error::HandleLost)
                })?;
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
    let send_body = gen_send_body(kind, method_id_expr, &lease_arr);
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
            #send_body
            let (rc, len) = send_result.map_err(|dead| {
                    S::server_id().set(self.server.get().with_generation(dead.new_generation()));
                    #err_type::from_wire(ipc::Error::HandleLost)
                })?;
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
            let argbuffer = unsafe { ipc::wire::assume_init_slice(&argbuffer, n) };
            let opcode = ipc::opcode(#kind_lit, #method_id_expr);
            let mut __retbuf_mem: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
                unsafe { core::mem::MaybeUninit::uninit().assume_init() };
            let retbuffer = unsafe { ipc::wire::as_mut_byte_slice(&mut __retbuf_mem) };
            let (rc, len) = ipc::kern::sys_send(
                server_id.get(),
                opcode,
                argbuffer,
                retbuffer,
                &mut leases,
            ).map_err(|dead| {
                server_id.set(server_id.get().with_generation(dead.new_generation()));
                #err_type::from_wire(ipc::Error::ServerDied)
            })?;
            let wire_result: core::result::Result<_, ipc::Error> = { #parse_reply };
            wire_result.map_err(#err_type::from_wire)
        }
    }
}

/// Generate the `sys_send` call + retbuffer setup for instance methods
/// (message and destructor).
fn gen_send_body(
    kind: u8,
    method_id_expr: &TokenStream2,
    lease_arr: &TokenStream2,
) -> TokenStream2 {
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);
    quote! {
        #lease_arr
        let argbuffer = unsafe { ipc::wire::assume_init_slice(&argbuffer, n) };
        let opcode = ipc::opcode(#kind_lit, #method_id_expr);
        let mut __retbuf_mem: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
            unsafe { core::mem::MaybeUninit::uninit().assume_init() };
        let retbuffer = unsafe { ipc::wire::as_mut_byte_slice(&mut __retbuf_mem) };
        let send_result = ipc::kern::sys_send(
            self.server.get(),
            opcode,
            argbuffer,
            retbuffer,
            &mut leases,
        );
    }
}
