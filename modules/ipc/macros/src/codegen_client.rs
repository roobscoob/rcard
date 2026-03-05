use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::{ConstructorReturn, MethodKind, ParsedMethod, ParsedParam};
use crate::util::to_pascal_case;

pub fn gen_client(trait_name: &Ident, methods: &[ParsedMethod], kind: u8) -> TokenStream2 {
    let handle_name = format_ident!("{}Handle", trait_name);
    let ctor_enum_name = format_ident!("{}CtorArgs", trait_name);
    let server_trait_name = format_ident!("{}Server", trait_name);
    let binding_struct_name = format_ident!("{}Server", trait_name);

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

    // Determine the handle's E type: the error type from the first Result(E) constructor,
    // or () if all constructors are Bare/OptionSelf.
    let handle_error_type: proc_macro2::TokenStream = constructors
        .iter()
        .find_map(|m| {
            if let Some(ConstructorReturn::Result(et)) = &m.ctor_return {
                Some(quote! { #et })
            } else {
                None
            }
        })
        .unwrap_or_else(|| quote! { () });

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

            let ctor_return = m
                .ctor_return
                .as_ref()
                .expect("constructor must have ctor_return");

            // Generate retbuffer size and handle extraction based on ctor_return.
            let (retbuffer_size, extract_handle) = match ctor_return {
                ConstructorReturn::Bare => (
                    quote! {
                        <core::result::Result<ipc::RawHandle, ipc::Error>
                            as hubpack::SerializedSize>::MAX_SIZE
                    },
                    quote! {
                        let (result, _) = hubpack::deserialize::<
                            core::result::Result<ipc::RawHandle, ipc::Error>
                        >(&retbuffer[..len])
                            .unwrap_or_else(|_| panic!(
                                "ipc reconstruct: server {:?} sent malformed constructor reply \
                                 ({} bytes received)",
                                self.server.get(), len,
                            ));
                        match result {
                            Ok(handle) => { self.handle.set(handle); }
                            Err(_) => return Err(ipc::Error::ServerDied),
                        }
                    },
                ),
                ConstructorReturn::Result(_) => (
                    // E is generic — can't use E::MAX_SIZE in an array length.
                    // Use the protocol maximum (256 bytes) instead.
                    quote! { ipc::HUBRIS_MESSAGE_SIZE_LIMIT },
                    quote! {
                        let (result, _) = hubpack::deserialize::<
                            core::result::Result<
                                core::result::Result<ipc::RawHandle, E>,
                                ipc::Error,
                            >
                        >(&retbuffer[..len])
                            .unwrap_or_else(|_| panic!(
                                "ipc reconstruct: server {:?} sent malformed constructor reply \
                                 ({} bytes received)",
                                self.server.get(), len,
                            ));
                        match result {
                            Ok(Ok(handle)) => { self.handle.set(handle); }
                            Ok(Err(e)) => return Err(ipc::Error::ReconstructionFailed(e)),
                            Err(_) => return Err(ipc::Error::ServerDied),
                        }
                    },
                ),
                ConstructorReturn::OptionSelf => (
                    quote! {
                        <core::result::Result<
                            core::option::Option<ipc::RawHandle>,
                            ipc::Error,
                        > as hubpack::SerializedSize>::MAX_SIZE
                    },
                    quote! {
                        let (result, _) = hubpack::deserialize::<
                            core::result::Result<
                                core::option::Option<ipc::RawHandle>,
                                ipc::Error,
                            >
                        >(&retbuffer[..len])
                            .unwrap_or_else(|_| panic!(
                                "ipc reconstruct: server {:?} sent malformed constructor reply \
                                 ({} bytes received)",
                                self.server.get(), len,
                            ));
                        match result {
                            Ok(Some(handle)) => { self.handle.set(handle); }
                            Ok(None) => return Err(ipc::Error::ReconstructionReturnedNone),
                            Err(_) => return Err(ipc::Error::ServerDied),
                        }
                    },
                ),
            };

            quote! {
                #destructure => {
                    #serialize
                    let mut retbuffer = [0u8; #retbuffer_size];
                    let mut leases = [];
                    let argbuffer = unsafe { argbuffer.get_unchecked(..n) };
                    let opcode = ipc::opcode(#kind_lit, #method_id_lit);
                    let (rc, len) = userlib::sys_send(
                        self.server.get(),
                        opcode,
                        argbuffer,
                        &mut retbuffer,
                        &mut leases,
                    ).map_err(|_| ipc::Error::ServerDied)?;
                    if rc != userlib::ResponseCode::SUCCESS {
                        panic!(
                            "ipc reconstruct: server {:?} sent unexpected non-SUCCESS response \
                             code; this indicates a protocol violation",
                            self.server.get(),
                        );
                    }
                    #extract_handle
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

            pub trait #server_trait_name {
                fn task_id() -> userlib::TaskId;
                fn server_id() -> &'static ipc::StaticTaskId;
            }

            #[derive(Clone, Copy)]
            enum #ctor_enum_name {
                #(#ctor_variants),*
            }

            pub struct #handle_name<S: #server_trait_name, E = #handle_error_type> {
                server: core::cell::Cell<userlib::TaskId>,
                handle: core::cell::Cell<ipc::RawHandle>,
                ctor: #ctor_enum_name,
                destroyed: core::cell::Cell<bool>,
                _server: core::marker::PhantomData<S>,
                _e: core::marker::PhantomData<E>,
            }

            impl<
                S: #server_trait_name,
                E: for<'de> serde::Deserialize<'de> + hubpack::SerializedSize,
            > #handle_name<S, E> {
                fn reconstruct(&self) -> core::result::Result<(), ipc::Error<E>> {
                    match self.ctor {
                        #(#reconstruct_arms)*
                    }
                }

                #(#method_impls)*
            }

            impl<S: #server_trait_name, E> Drop for #handle_name<S, E> {
                fn drop(&mut self) {
                    if self.destroyed.get() {
                        return;
                    }
                    // Send implicit destroy (0xFF) — best-effort, ignore errors.
                    let args: (ipc::RawHandle,) = (self.handle.get(),);
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

        #[macro_export]
        macro_rules! bind {
            ($name:ident = $slot:expr) => {
                #[doc(hidden)]
                struct #binding_struct_name;
                impl $crate::client::#server_trait_name for #binding_struct_name {
                    fn task_id() -> userlib::TaskId { $slot }
                    fn server_id() -> &'static ipc::StaticTaskId {
                        // Function-local static: each bind! invocation gets its
                        // own unique instance, so there is no name collision when
                        // bind! is used more than once in the same module.
                        static SERVER_ID: ipc::StaticTaskId = ipc::StaticTaskId::new($slot);
                        &SERVER_ID
                    }
                }
                type $name = $crate::client::#handle_name<#binding_struct_name>;
            };
        }
    }
}

fn gen_client_method(m: &ParsedMethod, kind: u8, ctor_enum_name: &Ident) -> TokenStream2 {
    let method_name = &m.name;
    let method_id = m.method_id;

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

    match m.kind {
        MethodKind::Constructor => {
            let ctor_return = m
                .ctor_return
                .as_ref()
                .expect("constructor must have ctor_return");
            gen_constructor(
                method_name,
                method_id,
                kind,
                &sig_params,
                &non_lease_params,
                &lease_params,
                ctor_enum_name,
                ctor_return,
            )
        }
        MethodKind::Message => gen_message(
            method_name,
            method_id,
            kind,
            &sig_params,
            &non_lease_params,
            &lease_params,
            m.return_type.as_ref(),
        ),
        MethodKind::StaticMessage => gen_static_message(
            method_name,
            method_id,
            kind,
            &sig_params,
            &non_lease_params,
            &lease_params,
            m.return_type.as_ref(),
        ),
        MethodKind::Destructor => gen_destructor(
            method_name,
            method_id,
            kind,
            &sig_params,
            &non_lease_params,
            &lease_params,
            m.return_type.as_ref(),
        ),
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
    ctor_return: &ConstructorReturn,
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

    let make_self = quote! {
        Self {
            server,
            handle: core::cell::Cell::new(handle),
            ctor: #ctor_value,
            destroyed: core::cell::Cell::new(false),
            _server: core::marker::PhantomData,
            _e: core::marker::PhantomData,
        }
    };

    match ctor_return {
        ConstructorReturn::Bare => {
            quote! {
                pub fn #method_name(
                    #(#sig_params),*
                ) -> core::result::Result<Self, ipc::Error> {
                    let server = core::cell::Cell::new(S::task_id());
                    #serialize
                    let mut retbuffer = [0u8;
                        <core::result::Result<ipc::RawHandle, ipc::Error>
                            as hubpack::SerializedSize>::MAX_SIZE];
                    #lease_arr
                    let argbuffer = unsafe { argbuffer.get_unchecked(..n) };
                    let opcode = ipc::opcode(#kind_lit, #method_id_lit);
                    let (rc, len) = userlib::sys_send(
                        server.get(),
                        opcode,
                        argbuffer,
                        &mut retbuffer,
                        &mut leases,
                    ).map_err(|_| ipc::Error::ServerDied)?;
                    if rc != userlib::ResponseCode::SUCCESS {
                        panic!(
                            "ipc: server {:?} sent unexpected non-SUCCESS response code",
                            server.get(),
                        );
                    }
                    let (result, _) = hubpack::deserialize::<
                        core::result::Result<ipc::RawHandle, ipc::Error>
                    >(&retbuffer[..len])
                        .unwrap_or_else(|_| panic!(
                            "ipc: server {:?} sent malformed constructor reply \
                             ({} bytes received)",
                            server.get(), len,
                        ));
                    match result {
                        Ok(handle) => Ok(#make_self),
                        Err(e) => Err(e),
                    }
                }
            }
        }
        ConstructorReturn::Result(error_type) => {
            quote! {
                pub fn #method_name(
                    #(#sig_params),*
                ) -> core::result::Result<core::result::Result<Self, #error_type>, ipc::Error> {
                    let server = core::cell::Cell::new(S::task_id());
                    #serialize
                    let mut retbuffer = [0u8;
                        <core::result::Result<
                            core::result::Result<ipc::RawHandle, #error_type>,
                            ipc::Error,
                        > as hubpack::SerializedSize>::MAX_SIZE];
                    #lease_arr
                    let argbuffer = unsafe { argbuffer.get_unchecked(..n) };
                    let opcode = ipc::opcode(#kind_lit, #method_id_lit);
                    let (rc, len) = userlib::sys_send(
                        server.get(),
                        opcode,
                        argbuffer,
                        &mut retbuffer,
                        &mut leases,
                    ).map_err(|_| ipc::Error::ServerDied)?;
                    if rc != userlib::ResponseCode::SUCCESS {
                        panic!(
                            "ipc: server {:?} sent unexpected non-SUCCESS response code",
                            server.get(),
                        );
                    }
                    let (result, _) = hubpack::deserialize::<
                        core::result::Result<
                            core::result::Result<ipc::RawHandle, #error_type>,
                            ipc::Error,
                        >
                    >(&retbuffer[..len])
                        .unwrap_or_else(|_| panic!(
                            "ipc: server {:?} sent malformed constructor reply \
                             ({} bytes received)",
                            server.get(), len,
                        ));
                    match result {
                        Ok(Ok(handle)) => Ok(Ok(#make_self)),
                        Ok(Err(e)) => Ok(Err(e)),
                        Err(ipc_err) => Err(ipc_err),
                    }
                }
            }
        }
        ConstructorReturn::OptionSelf => {
            quote! {
                pub fn #method_name(
                    #(#sig_params),*
                ) -> core::result::Result<core::option::Option<Self>, ipc::Error> {
                    let server = core::cell::Cell::new(S::task_id());
                    #serialize
                    let mut retbuffer = [0u8;
                        <core::result::Result<
                            core::option::Option<ipc::RawHandle>,
                            ipc::Error,
                        > as hubpack::SerializedSize>::MAX_SIZE];
                    #lease_arr
                    let argbuffer = unsafe { argbuffer.get_unchecked(..n) };
                    let opcode = ipc::opcode(#kind_lit, #method_id_lit);
                    let (rc, len) = userlib::sys_send(
                        server.get(),
                        opcode,
                        argbuffer,
                        &mut retbuffer,
                        &mut leases,
                    ).map_err(|_| ipc::Error::ServerDied)?;
                    if rc != userlib::ResponseCode::SUCCESS {
                        panic!(
                            "ipc: server {:?} sent unexpected non-SUCCESS response code",
                            server.get(),
                        );
                    }
                    let (result, _) = hubpack::deserialize::<
                        core::result::Result<
                            core::option::Option<ipc::RawHandle>,
                            ipc::Error,
                        >
                    >(&retbuffer[..len])
                        .unwrap_or_else(|_| panic!(
                            "ipc: server {:?} sent malformed constructor reply \
                             ({} bytes received)",
                            server.get(), len,
                        ));
                    match result {
                        Ok(Some(handle)) => Ok(Some(#make_self)),
                        Ok(None) => Ok(None),
                        Err(ipc_err) => Err(ipc_err),
                    }
                }
            }
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
    let parse_reply = gen_parse_reply(return_type, quote! { self.server.get() }, true);

    // For the retry path, we need to re-serialize because self.handle changed.
    let retry_serialize = gen_serialize_args(&arg_names, &arg_types, true);
    let retry_lease_arr = gen_lease_array(lease_params);
    let retry_send_body = gen_send_body(kind, method_id, &retry_lease_arr);

    let ret_type = match return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, ipc::Error<E>> },
        None => quote! { -> core::result::Result<(), ipc::Error<E>> },
    };

    quote! {
        pub fn #method_name(&self, #(#sig_params),*) #ret_type {
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
                    let (rc, len) = send_result.map_err(|_| ipc::Error::ServerDied)?;
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
    let parse_reply = gen_parse_reply(return_type, quote! { self.server.get() }, true);

    let ret_type = match return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, ipc::Error<E>> },
        None => quote! { -> core::result::Result<(), ipc::Error<E>> },
    };

    // On destructor, if server died the resource is already gone.
    // Just propagate the error. Set destroyed flag to prevent Drop from
    // sending a redundant implicit destroy.
    quote! {
        pub fn #method_name(self, #(#sig_params),*) #ret_type {
            self.destroyed.set(true);
            #serialize
            #send_body
            let (rc, len) = send_result.map_err(|_| ipc::Error::ServerDied)?;
            #parse_reply
        }
    }
}

fn gen_static_message(
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

    let serialize = gen_serialize_args(&arg_names, &arg_types, false);
    let lease_arr = gen_lease_array(lease_params);
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);
    let method_id_lit = proc_macro2::Literal::u8_suffixed(method_id);
    let parse_reply = gen_parse_reply(return_type, quote! { server_id.get() }, false);

    let ret_type = match return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, ipc::Error> },
        None => quote! { -> core::result::Result<(), ipc::Error> },
    };

    quote! {
        pub fn #method_name(#(#sig_params),*) #ret_type {
            let server_id = S::server_id();
            #serialize
            #lease_arr
            let argbuffer = unsafe { argbuffer.get_unchecked(..n) };
            let opcode = ipc::opcode(#kind_lit, #method_id_lit);
            let mut retbuffer = [0u8; ipc::HUBRIS_MESSAGE_SIZE_LIMIT];
            let send_result = userlib::sys_send(
                server_id.get(),
                opcode,
                argbuffer,
                &mut retbuffer,
                &mut leases,
            );
            match send_result {
                Ok((rc, len)) => {
                    #parse_reply
                }
                Err(dead) => {
                    server_id.set(server_id.get().with_generation(dead.new_generation()));
                    // Retry once with new generation.
                    let (rc, len) = userlib::sys_send(
                        server_id.get(),
                        opcode,
                        argbuffer,
                        &mut retbuffer,
                        &mut leases,
                    ).map_err(|_| ipc::Error::ServerDied)?;
                    #parse_reply
                }
            }
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
                let args: (ipc::RawHandle,) = (self.handle.get(),);
                let mut argbuffer = [0u8; <(ipc::RawHandle,) as hubpack::SerializedSize>::MAX_SIZE];
                let n = hubpack::serialize(&mut argbuffer, &args).unwrap_or(argbuffer.len());
            }
        } else {
            quote! {
                const _: () = assert!(
                    <(ipc::RawHandle, #(#arg_types,)*) as hubpack::SerializedSize>::MAX_SIZE
                        <= ipc::HUBRIS_MESSAGE_SIZE_LIMIT,
                    "argument types exceed Hubris message size limit (256 bytes)",
                );
                let args: (ipc::RawHandle, #(#arg_types,)*) = (self.handle.get(), #(#arg_names,)*);
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
            const _: () = assert!(
                <(#(#arg_types,)*) as hubpack::SerializedSize>::MAX_SIZE
                    <= ipc::HUBRIS_MESSAGE_SIZE_LIMIT,
                "argument types exceed Hubris message size limit (256 bytes)",
            );
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
        let mut retbuffer = [0u8; ipc::HUBRIS_MESSAGE_SIZE_LIMIT];
        let send_result = userlib::sys_send(
            self.server.get(),
            opcode,
            argbuffer,
            &mut retbuffer,
            &mut leases,
        );
    }
}

fn gen_parse_reply(
    return_type: Option<&syn::Type>,
    server_expr: TokenStream2,
    map_err: bool,
) -> TokenStream2 {
    let rc_check = quote! {
        if rc != userlib::ResponseCode::SUCCESS {
            panic!(
                "ipc: server {:?} sent unexpected non-SUCCESS response code; \
                 this indicates a protocol violation",
                #server_expr,
            );
        }
    };
    // Server always sends Result<T, ipc::Error> (no E).
    // For message/destructor methods (map_err = true) the return type is
    // Result<T, ipc::Error<E>>, so we map the error variant across.
    // For static messages (map_err = false) the return type is Result<T, ipc::Error>.
    let map = if map_err {
        quote! { .map_err(|e| match e {
            ipc::Error::ServerDied => ipc::Error::ServerDied,
            ipc::Error::ArenaFull => ipc::Error::ArenaFull,
            ipc::Error::InvalidHandle => ipc::Error::InvalidHandle,
            ipc::Error::ReconstructionReturnedNone => ipc::Error::ReconstructionReturnedNone,
            ipc::Error::ReconstructionFailed(_) => unreachable!(),
        }) }
    } else {
        quote! {}
    };
    if let Some(rt) = return_type {
        quote! {
            #rc_check
            const _: () = assert!(
                <core::result::Result<#rt, ipc::Error> as hubpack::SerializedSize>::MAX_SIZE
                    <= ipc::HUBRIS_MESSAGE_SIZE_LIMIT,
                "return type exceeds Hubris message size limit (256 bytes)",
            );
            let (result, _) = hubpack::deserialize::<
                core::result::Result<#rt, ipc::Error>
            >(&retbuffer[..len])
                .unwrap_or_else(|_| panic!(
                    "ipc: server {:?} sent malformed reply ({} bytes received)",
                    #server_expr, len,
                ));
            result #map
        }
    } else {
        quote! {
            #rc_check
            let (result, _) = hubpack::deserialize::<
                core::result::Result<(), ipc::Error>
            >(&retbuffer[..len])
                .unwrap_or_else(|_| panic!(
                    "ipc: server {:?} sent malformed reply ({} bytes received)",
                    #server_expr, len,
                ));
            result #map
        }
    }
}
