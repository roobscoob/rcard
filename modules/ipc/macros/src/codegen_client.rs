use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::{MethodKind, ParsedMethod, ParsedParam};
use crate::util::to_pascal_case;

pub fn gen_client(trait_name: &Ident, methods: &[ParsedMethod], kind: u8) -> TokenStream2 {
    let handle_name = format_ident!("{}Handle", trait_name);
    let ctor_enum_name = format_ident!("{}CtorArgs", trait_name);

    let constructors: Vec<&ParsedMethod> = methods
        .iter()
        .filter(|m| m.kind == MethodKind::Constructor)
        .collect();

    // Generate constructor enum variants (store non-lease params).
    let ctor_variants: Vec<TokenStream2> = constructors
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            let fields: Vec<TokenStream2> = m
                .params
                .iter()
                .filter(|p| !p.is_lease)
                .map(|p| {
                    let name = &p.name;
                    let ty = &p.ty;
                    quote! { #name: #ty }
                })
                .collect();
            if fields.is_empty() {
                quote! { #variant }
            } else {
                quote! { #variant { #(#fields),* } }
            }
        })
        .collect();

    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);

    // Generate reconstruct match arms.
    let reconstruct_arms: Vec<TokenStream2> = constructors
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            let method_id_lit = proc_macro2::Literal::u8_suffixed(m.method_id);

            let non_lease_params: Vec<&ParsedParam> =
                m.params.iter().filter(|p| !p.is_lease).collect();
            let arg_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
            let arg_types: Vec<_> = non_lease_params.iter().map(|p| &p.ty).collect();

            let destructure = if arg_names.is_empty() {
                quote! { #ctor_enum_name::#variant }
            } else {
                quote! { #ctor_enum_name::#variant { #(#arg_names),* } }
            };

            let serialize = gen_serialize_args(&arg_names, &arg_types, false);

            // Constructors with lease args can't fully reconstruct — send with empty leases.
            // The server may reject this, which propagates as an error.

            quote! {
                #destructure => {
                    #serialize
                    let mut retbuffer = [0u8; ipc::RawHandle::SIZE];
                    let mut leases = [];
                    let argbuffer = unsafe { argbuffer.get_unchecked(..n) };
                    let opcode = ipc::opcode(#kind_lit, #method_id_lit);
                    let (rc, len) = userlib::sys_send(
                        self.server.get(),
                        opcode,
                        argbuffer,
                        &mut retbuffer,
                        &mut leases,
                    )?;
                    self.handle = zerocopy::FromBytes::read_from_bytes(&retbuffer[..])
                        .unwrap_or(ipc::RawHandle(0));
                    Ok(())
                }
            }
        })
        .collect();

    let method_impls: Vec<TokenStream2> = methods
        .iter()
        .map(|m| gen_client_method(m, kind, &ctor_enum_name))
        .collect();

    quote! {
        pub mod client {
            use super::*;

            #[derive(Clone, Copy)]
            enum #ctor_enum_name {
                #(#ctor_variants),*
            }

            pub struct #handle_name {
                server: core::cell::Cell<userlib::TaskId>,
                handle: ipc::RawHandle,
                ctor: #ctor_enum_name,
                destroyed: core::cell::Cell<bool>,
            }

            impl #handle_name {
                fn reconstruct(&mut self) -> core::result::Result<(), userlib::TaskDeath> {
                    match self.ctor {
                        #(#reconstruct_arms)*
                    }
                }

                #(#method_impls)*
            }

            impl Drop for #handle_name {
                fn drop(&mut self) {
                    if self.destroyed.get() {
                        return;
                    }
                    // Send implicit destroy (0xFF) — best-effort, ignore errors.
                    let args: (ipc::RawHandle,) = (self.handle,);
                    let mut argbuffer = [0u8; <(ipc::RawHandle,) as hubpack::SerializedSize>::MAX_SIZE];
                    let n = hubpack::serialize(&mut argbuffer, &args).unwrap_or(argbuffer.len());
                    let argbuffer = unsafe { argbuffer.get_unchecked(..n) };
                    let opcode = ipc::opcode(#kind_lit, ipc::IMPLICIT_DESTROY_METHOD);
                    let mut retbuffer = [0u8; 0];
                    let mut leases = [];
                    let _ = userlib::sys_send(
                        self.server.get(),
                        opcode,
                        argbuffer,
                        &mut retbuffer,
                        &mut leases,
                    );
                }
            }
        }
    }
}

fn gen_client_method(m: &ParsedMethod, kind: u8, ctor_enum_name: &Ident) -> TokenStream2 {
    let method_name = &m.name;
    let method_id = m.method_id;

    let non_lease_params: Vec<&ParsedParam> =
        m.params.iter().filter(|p| !p.is_lease).collect();
    let lease_params: Vec<&ParsedParam> =
        m.params.iter().filter(|p| p.is_lease).collect();

    let sig_params: Vec<TokenStream2> = m
        .params
        .iter()
        .map(|p| {
            let pname = &p.name;
            let ty = &p.ty;
            quote! { #pname: #ty }
        })
        .collect();

    match m.kind {
        MethodKind::Constructor => {
            gen_constructor(method_name, method_id, kind, &sig_params, &non_lease_params, &lease_params, ctor_enum_name)
        }
        MethodKind::Message => {
            gen_message(method_name, method_id, kind, &sig_params, &non_lease_params, &lease_params, m.return_type.as_ref())
        }
        MethodKind::Destructor => {
            gen_destructor(method_name, method_id, kind, &sig_params, &non_lease_params, &lease_params, m.return_type.as_ref())
        }
    }
}

fn gen_constructor(
    method_name: &Ident,
    method_id: u8,
    kind: u8,
    sig_params: &[TokenStream2],
    non_lease_params: &[&ParsedParam],
    lease_params: &[&ParsedParam],
    ctor_enum_name: &Ident,
) -> TokenStream2 {
    let variant = format_ident!("{}", to_pascal_case(&method_name.to_string()));
    let arg_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let arg_types: Vec<_> = non_lease_params.iter().map(|p| &p.ty).collect();

    let serialize = gen_serialize_args(&arg_names, &arg_types, false);
    let lease_arr = gen_lease_array(lease_params);
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);
    let method_id_lit = proc_macro2::Literal::u8_suffixed(method_id);

    let ctor_value = if arg_names.is_empty() {
        quote! { #ctor_enum_name::#variant }
    } else {
        quote! { #ctor_enum_name::#variant { #(#arg_names: #arg_names.clone()),* } }
    };

    quote! {
        pub fn #method_name(
            server: userlib::TaskId,
            #(#sig_params),*
        ) -> core::result::Result<Self, userlib::TaskDeath> {
            let server = core::cell::Cell::new(server);
            #serialize
            let mut retbuffer = [0u8; ipc::RawHandle::SIZE];
            #lease_arr
            let argbuffer = unsafe { argbuffer.get_unchecked(..n) };
            let opcode = ipc::opcode(#kind_lit, #method_id_lit);
            let (rc, len) = userlib::sys_send(
                server.get(),
                opcode,
                argbuffer,
                &mut retbuffer,
                &mut leases,
            )?;
            let handle: ipc::RawHandle = zerocopy::FromBytes::read_from_bytes(&retbuffer[..])
                .unwrap_or(ipc::RawHandle(0));
            Ok(Self {
                server,
                handle,
                ctor: #ctor_value,
                destroyed: core::cell::Cell::new(false),
            })
        }
    }
}

fn gen_message(
    method_name: &Ident,
    method_id: u8,
    kind: u8,
    sig_params: &[TokenStream2],
    non_lease_params: &[&ParsedParam],
    lease_params: &[&ParsedParam],
    return_type: Option<&syn::Type>,
) -> TokenStream2 {
    let arg_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let arg_types: Vec<_> = non_lease_params.iter().map(|p| &p.ty).collect();

    let serialize = gen_serialize_args(&arg_names, &arg_types, true);
    let lease_arr = gen_lease_array(lease_params);
    let send_body = gen_send_body(kind, method_id, &lease_arr);
    let parse_reply = gen_parse_reply(return_type);

    // For the retry path, we need to re-serialize because self.handle changed.
    let retry_serialize = gen_serialize_args(&arg_names, &arg_types, true);
    let retry_lease_arr = gen_lease_array(lease_params);
    let retry_send_body = gen_send_body(kind, method_id, &retry_lease_arr);

    let ret_type = match return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, userlib::TaskDeath> },
        None => quote! { -> core::result::Result<(), userlib::TaskDeath> },
    };

    quote! {
        pub fn #method_name(&mut self, #(#sig_params),*) #ret_type {
            #serialize
            #send_body
            match send_result {
                Ok((rc, len)) => {
                    #parse_reply
                }
                Err(dead) => {
                    self.server.set(self.server.get().with_generation(dead.new_generation()));
                    self.reconstruct()?;
                    // Retry once with new handle.
                    #retry_serialize
                    #retry_send_body
                    let (rc, len) = send_result?;
                    #parse_reply
                }
            }
        }
    }
}

fn gen_destructor(
    method_name: &Ident,
    method_id: u8,
    kind: u8,
    sig_params: &[TokenStream2],
    non_lease_params: &[&ParsedParam],
    lease_params: &[&ParsedParam],
    return_type: Option<&syn::Type>,
) -> TokenStream2 {
    let arg_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let arg_types: Vec<_> = non_lease_params.iter().map(|p| &p.ty).collect();

    let serialize = gen_serialize_args(&arg_names, &arg_types, true);
    let lease_arr = gen_lease_array(lease_params);
    let send_body = gen_send_body(kind, method_id, &lease_arr);
    let parse_reply = gen_parse_reply(return_type);

    let ret_type = match return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, userlib::TaskDeath> },
        None => quote! { -> core::result::Result<(), userlib::TaskDeath> },
    };

    // On destructor, if server died the resource is already gone.
    // Just propagate the error. Set destroyed flag to prevent Drop from
    // sending a redundant implicit destroy.
    quote! {
        pub fn #method_name(self, #(#sig_params),*) #ret_type {
            self.destroyed.set(true);
            #serialize
            #send_body
            let (rc, len) = send_result?;
            #parse_reply
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn gen_serialize_args(
    arg_names: &[&Ident],
    arg_types: &[&syn::Type],
    include_handle: bool,
) -> TokenStream2 {
    if include_handle {
        if arg_types.is_empty() {
            quote! {
                let args: (ipc::RawHandle,) = (self.handle,);
                let mut argbuffer = [0u8; <(ipc::RawHandle,) as hubpack::SerializedSize>::MAX_SIZE];
                let n = hubpack::serialize(&mut argbuffer, &args).unwrap_or(argbuffer.len());
            }
        } else {
            quote! {
                let args: (ipc::RawHandle, #(#arg_types,)*) = (self.handle, #(#arg_names,)*);
                let mut argbuffer = [0u8; <(ipc::RawHandle, #(#arg_types,)*) as hubpack::SerializedSize>::MAX_SIZE];
                let n = hubpack::serialize(&mut argbuffer, &args).unwrap_or(argbuffer.len());
            }
        }
    } else if arg_types.is_empty() {
        quote! {
            let argbuffer = [];
            let n = 0usize;
        }
    } else {
        quote! {
            let args: (#(#arg_types,)*) = (#(#arg_names,)*);
            let mut argbuffer = [0u8; <(#(#arg_types,)*) as hubpack::SerializedSize>::MAX_SIZE];
            let n = hubpack::serialize(&mut argbuffer, &args).unwrap_or(argbuffer.len());
        }
    }
}

fn gen_lease_array(lease_params: &[&ParsedParam]) -> TokenStream2 {
    if lease_params.is_empty() {
        quote! { let mut leases = []; }
    } else {
        let exprs: Vec<TokenStream2> = lease_params
            .iter()
            .map(|p| {
                let pname = &p.name;
                if p.lease_mutable {
                    quote! { userlib::Lease::read_write(zerocopy::IntoBytes::as_mut_bytes(#pname)) }
                } else {
                    quote! { userlib::Lease::read_only(zerocopy::IntoBytes::as_bytes(#pname)) }
                }
            })
            .collect();
        quote! { let mut leases = [#(#exprs),*]; }
    }
}

fn gen_send_body(kind: u8, method_id: u8, lease_arr: &TokenStream2) -> TokenStream2 {
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);
    let method_id_lit = proc_macro2::Literal::u8_suffixed(method_id);
    quote! {
        #lease_arr
        let argbuffer = unsafe { argbuffer.get_unchecked(..n) };
        let opcode = ipc::opcode(#kind_lit, #method_id_lit);
        let mut retbuffer = [0u8; 64];
        let send_result = userlib::sys_send(
            self.server.get(),
            opcode,
            argbuffer,
            &mut retbuffer,
            &mut leases,
        );
    }
}

fn gen_parse_reply(return_type: Option<&syn::Type>) -> TokenStream2 {
    if let Some(rt) = return_type {
        quote! {
            let (value, _) = hubpack::deserialize::<#rt>(&retbuffer[..len])
                .unwrap_or_else(|_| panic!("ipc deserialize failed"));
            Ok(value)
        }
    } else {
        quote! {
            let _ = (rc, len);
            Ok(())
        }
    }
}
