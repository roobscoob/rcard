use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::{
    CloneMode, ConstructorReturn, HandleMode, MethodKind, ParsedMethod, ParsedParam, ResourceAttr,
};
use crate::util::{replace_ident_in_type, to_pascal_case, to_snake_case};

// ===========================================================================
// Concrete client (for resources with constructors / arena)
// ===========================================================================

pub fn gen_client(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    attrs: &ResourceAttr,
) -> TokenStream2 {
    let kind = attrs.kind;
    let handle_name = format_ident!("{}Handle", trait_name);
    let ctor_enum_name = format_ident!("{}CtorArgs", trait_name);
    let server_trait_name = format_ident!("{}Server", trait_name);
    let binding_struct_name = format_ident!("{}Server", trait_name);
    let bind_macro_name = format_ident!("bind_{}", to_snake_case(&trait_name.to_string()));

    let constructors: Vec<&ParsedMethod> = methods
        .iter()
        .filter(|m| m.kind == MethodKind::Constructor)
        .collect();

    // Generate constructor enum variants (store non-lease, non-handle params).
    let ctor_variants: Vec<TokenStream2> = constructors
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            let fields: Vec<TokenStream2> = m
                .params
                .iter()
                .filter(|p| !p.is_lease && p.handle_mode.is_none())
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

    // Determine the handle's E type.
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

    let enum_name = format_ident!("{}Op", trait_name);

    // Generate reconstruct match arms.
    let reconstruct_arms: Vec<TokenStream2> = constructors
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            let method_id_expr = quote! { #enum_name::#variant as u8 };

            let non_lease_non_handle_params: Vec<&ParsedParam> = m
                .params
                .iter()
                .filter(|p| !p.is_lease && p.handle_mode.is_none())
                .collect();
            let arg_names: Vec<&Ident> =
                non_lease_non_handle_params.iter().map(|p| &p.name).collect();
            let arg_types: Vec<TokenStream2> =
                non_lease_non_handle_params.iter().map(|p| {
                    let ty = &p.ty;
                    quote! { #ty }
                }).collect();

            let destructure = if arg_names.is_empty() {
                quote! { #ctor_enum_name::#variant }
            } else {
                quote! { #ctor_enum_name::#variant { #(#arg_names),* } }
            };

            let serialize = gen_serialize_wire(&arg_names, &arg_types, None);

            let ctor_return = m
                .ctor_return
                .as_ref()
                .expect("constructor must have ctor_return");

            let wire_type = ctor_wire_type(ctor_return);
            let retbuffer_size = ctor_retbuffer_size_reconstruct(ctor_return);

            let extract_handle = match ctor_return {
                ConstructorReturn::Bare => quote! {
                    match result {
                        Ok(handle) => { self.handle.set(handle); }
                        Err(_) => return Err(ipc::Error::ServerDied),
                    }
                },
                ConstructorReturn::Result(_) => quote! {
                    match result {
                        Ok(Ok(handle)) => { self.handle.set(handle); }
                        Ok(Err(e)) => return Err(ipc::Error::ReconstructionFailed(e)),
                        Err(_) => return Err(ipc::Error::ServerDied),
                    }
                },
                ConstructorReturn::OptionSelf => quote! {
                    match result {
                        Ok(Some(handle)) => { self.handle.set(handle); }
                        Ok(None) => return Err(ipc::Error::ReconstructionReturnedNone),
                        Err(_) => return Err(ipc::Error::ServerDied),
                    }
                },
            };

            quote! {
                #destructure => {
                    #serialize
                    let mut retbuffer = [0u8; #retbuffer_size];
                    let mut leases = [];
                    let argbuffer = &argbuffer[..n];
                    let opcode = ipc::opcode(#kind_lit, #method_id_expr);
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
                    let (result, _) = hubpack::deserialize::<#wire_type>(&retbuffer[..len])
                        .unwrap_or_else(|_| panic!(
                            "ipc reconstruct: server {:?} sent malformed constructor reply \
                             ({} bytes received)",
                            self.server.get(), len,
                        ));
                    #extract_handle
                    Ok(())
                }
            }
        })
        .collect();

    let method_impls: Vec<TokenStream2> = methods
        .iter()
        .map(|m| gen_client_method(m, kind, &enum_name, &ctor_enum_name, &handle_error_type))
        .collect();

    // Generate Transferable impl.
    let transferable_impl = gen_transferable_impl(&handle_name, &server_trait_name, kind);

    // Generate Cloneable impl if clone = refcount.
    let cloneable_impl = if attrs.clone_mode == Some(CloneMode::Refcount) {
        gen_cloneable_impl(&handle_name, &server_trait_name, kind)
    } else {
        quote! {}
    };

    let mod_name = format_ident!("{}_client", to_snake_case(&trait_name.to_string()));

    quote! {
        pub mod #mod_name {
            use super::*;

            pub trait #server_trait_name {
                fn task_id() -> userlib::TaskId;
                fn server_id() -> &'static ipc::StaticTaskId;
            }

            #[derive(Clone, Copy)]
            enum #ctor_enum_name {
                #(#ctor_variants),*
            }

            pub struct #handle_name<S: #server_trait_name> {
                server: core::cell::Cell<userlib::TaskId>,
                handle: core::cell::Cell<ipc::RawHandle>,
                ctor: core::option::Option<#ctor_enum_name>,
                destroyed: core::cell::Cell<bool>,
                _server: core::marker::PhantomData<S>,
            }

            impl<S: #server_trait_name> #handle_name<S> {
                /// Adopt a raw handle (e.g. from a transfer). No auto-reconstruction
                /// on server death — if the server dies, operations return `ServerDied`.
                pub fn from_raw(handle: ipc::RawHandle) -> Self {
                    Self {
                        server: core::cell::Cell::new(S::task_id()),
                        handle: core::cell::Cell::new(handle),
                        ctor: None,
                        destroyed: core::cell::Cell::new(false),
                        _server: core::marker::PhantomData,
                    }
                }

                fn reconstruct(&self) -> core::result::Result<(), ipc::Error<#handle_error_type>> {
                    match &self.ctor {
                        None => Err(ipc::Error::ServerDied),
                        Some(ctor) => match *ctor {
                            #(#reconstruct_arms)*
                        },
                    }
                }

                /// Get the underlying raw handle.
                pub fn raw(&self) -> ipc::RawHandle {
                    self.handle.get()
                }

                /// Get the kind byte for this resource.
                pub const fn kind() -> u8 {
                    #kind_lit
                }

                #(#method_impls)*
            }

            #transferable_impl
            #cloneable_impl

            impl<S: #server_trait_name> Drop for #handle_name<S> {
                fn drop(&mut self) {
                    if self.destroyed.get() {
                        return;
                    }
                    // Send implicit destroy (0xFF) — best-effort, ignore errors.
                    let args: (ipc::RawHandle,) = (self.handle.get(),);
                    let mut argbuffer = [0u8; <(ipc::RawHandle,) as hubpack::SerializedSize>::MAX_SIZE];
                    let n = hubpack::serialize(&mut argbuffer, &args).expect("ipc: serialize failed");
                    let argbuffer = &argbuffer[..n];
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
        macro_rules! #bind_macro_name {
            ($name:ident = $slot:expr) => {
                #[doc(hidden)]
                struct #binding_struct_name;
                impl $crate::#mod_name::#server_trait_name for #binding_struct_name {
                    fn task_id() -> userlib::TaskId { $slot }
                    fn server_id() -> &'static ipc::StaticTaskId {
                        static SERVER_ID: ipc::StaticTaskId = ipc::StaticTaskId::new($slot);
                        &SERVER_ID
                    }
                }
                type $name = $crate::#mod_name::#handle_name<#binding_struct_name>;
            };
        }

    pub use #mod_name::*;
    }
}
// ===========================================================================
// Dynamic client (for interface-only traits, no arena)
// ==========================================================================

pub fn gen_dyn_client(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    _attrs: &ResourceAttr,
) -> TokenStream2 {
    let dyn_name = format_ident!("{}Dyn", trait_name);
    let mod_name = format_ident!("{}_client", to_snake_case(&trait_name.to_string()));
    let method_impls: Vec<TokenStream2> = methods
        .iter()
        .filter(|m| m.kind == MethodKind::Message)
        .map(|m| gen_dyn_method(m))
        .collect();

    let static_method_impls: Vec<TokenStream2> = methods
        .iter()
        .filter(|m| m.kind == MethodKind::StaticMessage)
        .map(|m| gen_dyn_static_method(m))
        .collect();

    quote! {
        pub mod #mod_name {
            use super::*;

            /// Dynamic client for any server implementing this interface.
            /// Created from a `DynHandle` received via handle forwarding.
            pub struct #dyn_name {
                server: core::cell::Cell<userlib::TaskId>,
                kind: u8,
                handle: core::cell::Cell<ipc::RawHandle>,
            }

            impl #dyn_name {
                /// Create a dynamic client from a `DynHandle`.
                ///
                /// The `DynHandle` carries the server's TaskId (with generation).
                /// If the server has restarted since the handle was created, the
                /// first IPC call will detect this and update the generation.
                pub fn from_dyn_handle(dh: ipc::DynHandle) -> Self {
                    Self {
                        server: core::cell::Cell::new(dh.task_id()),
                        kind: dh.kind,
                        handle: core::cell::Cell::new(dh.handle),
                    }
                }

                /// Get the underlying raw handle.
                pub fn raw(&self) -> ipc::RawHandle {
                    self.handle.get()
                }

                /// Get the kind byte.
                pub fn kind(&self) -> u8 {
                    self.kind
                }

                #(#method_impls)*
                #(#static_method_impls)*
            }

            impl ipc::Transferable for #dyn_name {
                fn transfer_to(self, new_owner: userlib::TaskId) -> core::result::Result<ipc::DynHandle, ipc::Error> {
                    let args: (ipc::RawHandle, u16) = (self.handle.get(), new_owner.task_index());
                    let mut argbuffer = [0u8;
                        <(ipc::RawHandle, u16) as hubpack::SerializedSize>::MAX_SIZE];
                    let n = hubpack::serialize(&mut argbuffer, &args).expect("ipc: serialize failed");
                    let mut retbuffer = [0u8;
                        <core::result::Result<(), ipc::Error> as hubpack::SerializedSize>::MAX_SIZE];
                    let mut leases = [];
                    let opcode = ipc::opcode(self.kind, ipc::TRANSFER_METHOD);
                    let (rc, len) = userlib::sys_send(
                        self.server.get(),
                        opcode,
                        &argbuffer[..n],
                        &mut retbuffer,
                        &mut leases,
                    ).map_err(|_| ipc::Error::ServerDied)?;
                    if rc != userlib::ResponseCode::SUCCESS {
                        panic!("ipc: transfer got non-SUCCESS response code");
                    }
                    let (result, _) = hubpack::deserialize::<
                        core::result::Result<(), ipc::Error>
                    >(&retbuffer[..len])
                        .unwrap_or_else(|_| panic!("ipc: malformed transfer reply"));
                    result?;
                    let dh = ipc::DynHandle {
                        server_id: u16::from(self.server.get()),
                        kind: self.kind,
                        handle: self.handle.get(),
                    };
                    // Prevent Drop from sending 0xFF — ownership transferred.
                    core::mem::forget(self);
                    Ok(dh)
                }
            }

            impl Drop for #dyn_name {
                fn drop(&mut self) {
                    let args: (ipc::RawHandle,) = (self.handle.get(),);
                    let mut argbuffer = [0u8;
                        <(ipc::RawHandle,) as hubpack::SerializedSize>::MAX_SIZE];
                    let n = hubpack::serialize(&mut argbuffer, &args).expect("ipc: serialize failed");
                    let opcode = ipc::opcode(self.kind, ipc::IMPLICIT_DESTROY_METHOD);
                    let mut retbuffer = [0u8; 0];
                    let mut leases = [];
                    let _ = userlib::sys_send(
                        self.server.get(),
                        opcode,
                        &argbuffer[..n],
                        &mut retbuffer,
                        &mut leases,
                    );
                }
            }
        }

        pub use #mod_name::*;
    }
}

// ===========================================================================
// Transferable & Cloneable impls for concrete handles
// ===========================================================================

fn gen_transferable_impl(
    handle_name: &Ident,
    server_trait_name: &Ident,
    kind: u8,
) -> TokenStream2 {
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);

    quote! {
        impl<S: #server_trait_name> ipc::Transferable for #handle_name<S> {
            fn transfer_to(self, new_owner: userlib::TaskId) -> core::result::Result<ipc::DynHandle, ipc::Error> {
                let args: (ipc::RawHandle, u16) = (self.handle.get(), new_owner.task_index());
                let mut argbuffer = [0u8;
                    <(ipc::RawHandle, u16) as hubpack::SerializedSize>::MAX_SIZE];
                let n = hubpack::serialize(&mut argbuffer, &args).expect("ipc: serialize failed");
                let mut retbuffer = [0u8;
                    <core::result::Result<(), ipc::Error> as hubpack::SerializedSize>::MAX_SIZE];
                let mut leases = [];
                let opcode = ipc::opcode(#kind_lit, ipc::TRANSFER_METHOD);
                let (rc, len) = userlib::sys_send(
                    self.server.get(),
                    opcode,
                    &argbuffer[..n],
                    &mut retbuffer,
                    &mut leases,
                ).map_err(|_| ipc::Error::ServerDied)?;
                if rc != userlib::ResponseCode::SUCCESS {
                    panic!("ipc: transfer got non-SUCCESS response code");
                }
                let (result, _) = hubpack::deserialize::<
                    core::result::Result<(), ipc::Error>
                >(&retbuffer[..len])
                    .unwrap_or_else(|_| panic!("ipc: malformed transfer reply"));
                result?;
                let dh = ipc::DynHandle {
                    server_id: u16::from(self.server.get()),
                    kind: #kind_lit,
                    handle: self.handle.get(),
                };
                // Prevent Drop from sending 0xFF — ownership transferred successfully.
                core::mem::forget(self);
                Ok(dh)
            }
        }
    }
}

fn gen_cloneable_impl(
    handle_name: &Ident,
    server_trait_name: &Ident,
    kind: u8,
) -> TokenStream2 {
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);

    quote! {
        impl<S: #server_trait_name> ipc::Cloneable for #handle_name<S> {
            fn clone_for(&self, new_owner: userlib::TaskId) -> core::result::Result<ipc::DynHandle, ipc::Error> {
                let args: (ipc::RawHandle, u16) = (self.handle.get(), new_owner.task_index());
                let mut argbuffer = [0u8;
                    <(ipc::RawHandle, u16) as hubpack::SerializedSize>::MAX_SIZE];
                let n = hubpack::serialize(&mut argbuffer, &args).expect("ipc: serialize failed");
                let mut retbuffer = [0u8;
                    <core::result::Result<ipc::RawHandle, ipc::Error>
                        as hubpack::SerializedSize>::MAX_SIZE];
                let mut leases = [];
                let opcode = ipc::opcode(#kind_lit, ipc::CLONE_METHOD);
                let (rc, len) = userlib::sys_send(
                    self.server.get(),
                    opcode,
                    &argbuffer[..n],
                    &mut retbuffer,
                    &mut leases,
                ).map_err(|_| ipc::Error::ServerDied)?;
                if rc != userlib::ResponseCode::SUCCESS {
                    panic!("ipc: clone got non-SUCCESS response code");
                }
                let (result, _) = hubpack::deserialize::<
                    core::result::Result<ipc::RawHandle, ipc::Error>
                >(&retbuffer[..len])
                    .unwrap_or_else(|_| panic!("ipc: malformed clone reply"));
                let new_handle = result?;
                Ok(ipc::DynHandle {
                    server_id: u16::from(self.server.get()),
                    kind: #kind_lit,
                    handle: new_handle,
                })
            }
        }
    }
}

// ===========================================================================
// Per-method client codegen
// ===========================================================================

fn gen_client_method(
    m: &ParsedMethod,
    kind: u8,
    enum_name: &Ident,
    ctor_enum_name: &Ident,
    error_type: &TokenStream2,
) -> TokenStream2 {
    let method_name = &m.name;
    let variant = format_ident!("{}", to_pascal_case(&method_name.to_string()));
    let method_id_expr = quote! { #enum_name::#variant as u8 };

    let non_lease_params: Vec<&ParsedParam> = m.params.iter().filter(|p| !p.is_lease).collect();
    let lease_params: Vec<&ParsedParam> = m.params.iter().filter(|p| p.is_lease).collect();

    let sig_params: Vec<TokenStream2> = m
        .params
        .iter()
        .map(|p| {
            let pname = &p.name;
            if p.handle_mode.is_some() {
                // Handle params: accept impl Transferable (move) or impl Cloneable (clone).
                match p.handle_mode {
                    Some(HandleMode::Move) => {
                        quote! { #pname: impl ipc::Transferable }
                    }
                    Some(HandleMode::Clone) => {
                        quote! { #pname: &impl ipc::Cloneable }
                    }
                    None => unreachable!(),
                }
            } else {
                let ty = &p.ty;
                quote! { #pname: #ty }
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
                ctor_enum_name,
                ctor_return,
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
            error_type,
            m.constructs.as_ref(),
        ),
        MethodKind::StaticMessage => gen_static_message(
            method_name,
            &method_id_expr,
            kind,
            &sig_params,
            &non_lease_params,
            &lease_params,
            m.return_type.as_ref(),
        ),
        MethodKind::Destructor => gen_destructor(
            method_name,
            &method_id_expr,
            kind,
            &sig_params,
            &non_lease_params,
            &lease_params,
            m.return_type.as_ref(),
            error_type,
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
    ctor_enum_name: &Ident,
    ctor_return: &ConstructorReturn,
) -> TokenStream2 {
    let variant = format_ident!("{}", to_pascal_case(&method_name.to_string()));

    // For ctor args storage, exclude lease and handle params.
    let storable_params: Vec<&&ParsedParam> = non_lease_params
        .iter()
        .filter(|p| p.handle_mode.is_none())
        .collect();
    let storable_names: Vec<&Ident> = storable_params.iter().map(|p| &p.name).collect();

    // For serialization, we need to handle #[handle(move)] params specially.
    let ctor_server_expr = quote! { server.get() };
    let handle_transfer_stmts = gen_handle_transfer_stmts(non_lease_params, &ctor_server_expr);

    let wire_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let wire_types: Vec<TokenStream2> = non_lease_params.iter().map(|p| wire_type_for(p)).collect();

    let serialize = gen_serialize_wire(&wire_names, &wire_types, None);
    let lease_arr = gen_lease_array(lease_params);
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);

    let ctor_value = if storable_names.is_empty() {
        quote! { #ctor_enum_name::#variant }
    } else {
        quote! { #ctor_enum_name::#variant { #(#storable_names: #storable_names.clone()),* } }
    };

    let make_self = quote! {
        Self {
            server,
            handle: core::cell::Cell::new(handle),
            ctor: Some(#ctor_value),
            destroyed: core::cell::Cell::new(false),
            _server: core::marker::PhantomData,
        }
    };

    // Variant-specific pieces.
    let wire_type = ctor_wire_type(ctor_return);
    let retbuf_size = ctor_retbuffer_size(ctor_return);

    let (fn_ret, map_result) = match ctor_return {
        ConstructorReturn::Bare => (
            quote! { core::result::Result<Self, ipc::Error> },
            quote! {
                match result {
                    Ok(handle) => Ok(#make_self),
                    Err(e) => Err(e),
                }
            },
        ),
        ConstructorReturn::Result(error_type) => (
            quote! { core::result::Result<core::result::Result<Self, #error_type>, ipc::Error> },
            quote! {
                match result {
                    Ok(Ok(handle)) => Ok(Ok(#make_self)),
                    Ok(Err(e)) => Ok(Err(e)),
                    Err(ipc_err) => Err(ipc_err),
                }
            },
        ),
        ConstructorReturn::OptionSelf => (
            quote! { core::result::Result<core::option::Option<Self>, ipc::Error> },
            quote! {
                match result {
                    Ok(Some(handle)) => Ok(Some(#make_self)),
                    Ok(None) => Ok(None),
                    Err(ipc_err) => Err(ipc_err),
                }
            },
        ),
    };

    quote! {
        pub fn #method_name(
            #(#sig_params),*
        ) -> #fn_ret {
            let server = core::cell::Cell::new(S::task_id());
            #handle_transfer_stmts
            #serialize
            let mut retbuffer = [0u8; #retbuf_size];
            #lease_arr
            let argbuffer = &argbuffer[..n];
            let opcode = ipc::opcode(#kind_lit, #method_id_expr);
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
            let (result, _) = hubpack::deserialize::<#wire_type>(&retbuffer[..len])
                .unwrap_or_else(|_| panic!(
                    "ipc: server {:?} sent malformed constructor reply \
                     ({} bytes received)",
                    server.get(), len,
                ));
            #map_result
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
    handle_error_type: &TokenStream2,
    constructs: Option<&(Ident, Ident)>,
) -> TokenStream2 {
    let self_server_expr = quote! { self.server.get() };
    let handle_transfer_stmts = gen_handle_transfer_stmts(non_lease_params, &self_server_expr);

    let wire_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let wire_types: Vec<TokenStream2> = non_lease_params.iter().map(|p| wire_type_for(p)).collect();

    let handle_expr = quote! { self.handle.get() };
    let serialize = gen_serialize_wire(&wire_names, &wire_types, Some(&handle_expr));
    let lease_arr = gen_lease_array(lease_params);
    let send_body = gen_send_body(kind, method_id_expr, &lease_arr);

    // For the retry path, re-serialize because self.handle changed.
    let retry_serialize = gen_serialize_wire(&wire_names, &wire_types, Some(&handle_expr));
    let retry_lease_arr = gen_lease_array(lease_params);
    let retry_send_body = gen_send_body(kind, method_id_expr, &retry_lease_arr);

    if let Some((trait_name, generic_ident)) = constructs {
        // This message constructs a handle of a different resource type.
        // The wire reply contains RawHandle where the user-facing type is
        // `TraitHandle<GenericIdent>`. We deserialize the wire type (with
        // RawHandle), then map into the handle wrapper.
        let handle_type = format_ident!("{}Handle", trait_name);
        let server_trait = format_ident!("{}Server", trait_name);

        // Build the wire return type (FS → RawHandle) and user return type (FS → Handle<FS>).
        let (wire_rt, user_rt) = match return_type {
            Some(rt) => {
                let wire = replace_ident_in_type(rt, generic_ident, &quote! { ipc::RawHandle });
                let user = replace_ident_in_type(rt, generic_ident, &quote! { #handle_type<#generic_ident> });
                (quote! { #wire }, quote! { #user })
            }
            None => (quote! { () }, quote! { () }),
        };

        let parse_reply = gen_parse_reply(Some(&syn::parse2(wire_rt.clone()).unwrap()), quote! { self.server.get() }, true);

        let map_handles = gen_constructs_map(return_type, generic_ident, &handle_type);

        let ret_type = quote! {
            -> core::result::Result<#user_rt, ipc::Error<#handle_error_type>>
        };

        quote! {
            pub fn #method_name<#generic_ident: #server_trait>(&self, #(#sig_params),*) #ret_type {
                #handle_transfer_stmts
                #serialize
                #send_body
                match send_result {
                    Ok((rc, len)) => {
                        let wire_result = { #parse_reply };
                        wire_result.map(|v| { #map_handles })
                    }
                    Err(dead) => {
                        self.server.set(self.server.get().with_generation(dead.new_generation()));
                        self.reconstruct()?;
                        #retry_serialize
                        #retry_send_body
                        let (rc, len) = send_result.map_err(|_| ipc::Error::ServerDied)?;
                        let wire_result = { #parse_reply };
                        wire_result.map(|v| { #map_handles })
                    }
                }
            }
        }
    } else {
        // Normal message — no constructs.
        let parse_reply = gen_parse_reply(return_type, quote! { self.server.get() }, true);

        let ret_type = match return_type {
            Some(rt) => quote! { -> core::result::Result<#rt, ipc::Error<#handle_error_type>> },
            None => quote! { -> core::result::Result<(), ipc::Error<#handle_error_type>> },
        };

        quote! {
            pub fn #method_name(&self, #(#sig_params),*) #ret_type {
                #handle_transfer_stmts
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
}

fn gen_destructor(
    method_name: &Ident,
    method_id_expr: &TokenStream2,
    kind: u8,
    sig_params: &[TokenStream2],
    non_lease_params: &[&ParsedParam],
    lease_params: &[&ParsedParam],
    return_type: Option<&syn::Type>,
    handle_error_type: &TokenStream2,
) -> TokenStream2 {
    let self_server_expr = quote! { self.server.get() };
    let handle_transfer_stmts = gen_handle_transfer_stmts(non_lease_params, &self_server_expr);

    let wire_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let wire_types: Vec<TokenStream2> = non_lease_params.iter().map(|p| wire_type_for(p)).collect();

    let handle_expr = quote! { self.handle.get() };
    let serialize = gen_serialize_wire(&wire_names, &wire_types, Some(&handle_expr));
    let lease_arr = gen_lease_array(lease_params);
    let send_body = gen_send_body(kind, method_id_expr, &lease_arr);
    let parse_reply = gen_parse_reply(return_type, quote! { self.server.get() }, true);

    let ret_type = match return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, ipc::Error<#handle_error_type>> },
        None => quote! { -> core::result::Result<(), ipc::Error<#handle_error_type>> },
    };

    quote! {
        pub fn #method_name(self, #(#sig_params),*) #ret_type {
            self.destroyed.set(true);
            #handle_transfer_stmts
            #serialize
            #send_body
            let (rc, len) = send_result.map_err(|_| ipc::Error::ServerDied)?;
            #parse_reply
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
) -> TokenStream2 {
    let server_expr = quote! { server_id.get() };
    let handle_transfer_stmts = gen_handle_transfer_stmts(non_lease_params, &server_expr);

    let wire_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let wire_types: Vec<TokenStream2> = non_lease_params.iter().map(|p| wire_type_for(p)).collect();

    let serialize = gen_serialize_wire(&wire_names, &wire_types, None);
    let lease_arr = gen_lease_array(lease_params);
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);
    let parse_reply = gen_parse_reply(return_type, quote! { server_id.get() }, false);

    let ret_type = match return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, ipc::Error> },
        None => quote! { -> core::result::Result<(), ipc::Error> },
    };

    quote! {
        pub fn #method_name(#(#sig_params),*) #ret_type {
            let server_id = S::server_id();
            #handle_transfer_stmts
            #serialize
            #lease_arr
            let argbuffer = &argbuffer[..n];
            let opcode = ipc::opcode(#kind_lit, #method_id_expr);
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

// ===========================================================================
// Dynamic client method codegen
// ===========================================================================

fn gen_dyn_method(m: &ParsedMethod) -> TokenStream2 {
    let method_name = &m.name;
    let method_id_lit = proc_macro2::Literal::u8_suffixed(m.method_id);

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
    let handle_transfer_stmts = gen_handle_transfer_stmts(&non_lease_params, &server_expr);

    let wire_names: Vec<&Ident> = non_lease_params.iter().map(|p| &p.name).collect();
    let wire_types: Vec<TokenStream2> = non_lease_params.iter().map(|p| wire_type_for(p)).collect();

    let handle_expr = quote! { self.handle.get() };
    let serialize = gen_serialize_wire(&wire_names, &wire_types, Some(&handle_expr));
    let lease_arr = gen_lease_array(&lease_params);

    let ret_type = match &m.return_type {
        Some(rt) => quote! { -> core::result::Result<#rt, ipc::Error> },
        None => quote! { -> core::result::Result<(), ipc::Error> },
    };

    let parse_reply = gen_parse_reply(m.return_type.as_ref(), quote! { self.server.get() }, false);

    quote! {
        pub fn #method_name(&self, #(#sig_params),*) #ret_type {
            #handle_transfer_stmts
            #serialize
            #lease_arr
            let argbuffer = &argbuffer[..n];
            let opcode = ipc::opcode(self.kind, #method_id_lit);
            let mut retbuffer = [0u8; ipc::HUBRIS_MESSAGE_SIZE_LIMIT];
            let send_result = userlib::sys_send(
                self.server.get(),
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
                    self.server.set(self.server.get().with_generation(dead.new_generation()));
                    let (rc, len) = userlib::sys_send(
                        self.server.get(),
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

fn gen_dyn_static_method(_m: &ParsedMethod) -> TokenStream2 {
    // Static methods on dynamic clients need a server binding — not yet supported.
    quote! {}
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Determine the wire type for a param. Handle params become RawHandle or DynHandle.
fn wire_type_for(p: &ParsedParam) -> TokenStream2 {
    if p.handle_mode.is_some() {
        if p.impl_trait_name.is_some() {
            quote! { ipc::DynHandle }
        } else {
            quote! { ipc::RawHandle }
        }
    } else {
        let ty = &p.ty;
        quote! { #ty }
    }
}

/// Generate let-bindings that transfer/clone handle params before serialization.
/// `server_expr` is the expression for the target server's TaskId
/// (e.g. `self.server.get()` for instance methods, `server.get()` for constructors).
fn gen_handle_transfer_stmts(
    params: &[&ParsedParam],
    server_expr: &TokenStream2,
) -> TokenStream2 {
    let stmts: Vec<TokenStream2> = params
        .iter()
        .filter_map(|p| {
            let pname = &p.name;
            match p.handle_mode {
                Some(HandleMode::Move) => {
                    if p.impl_trait_name.is_some() {
                        Some(quote! {
                            let #pname: ipc::DynHandle = ipc::Transferable::transfer_to(
                                #pname,
                                #server_expr,
                            ).map_err(|_| ipc::Error::ServerDied)?;
                        })
                    } else {
                        Some(quote! {
                            let __dh = ipc::Transferable::transfer_to(
                                #pname,
                                #server_expr,
                            ).map_err(|_| ipc::Error::ServerDied)?;
                            let #pname: ipc::RawHandle = __dh.handle;
                        })
                    }
                }
                Some(HandleMode::Clone) => {
                    if p.impl_trait_name.is_some() {
                        Some(quote! {
                            let #pname: ipc::DynHandle = ipc::Cloneable::clone_for(
                                #pname,
                                #server_expr,
                            ).map_err(|_| ipc::Error::ServerDied)?;
                        })
                    } else {
                        Some(quote! {
                            let __dh = ipc::Cloneable::clone_for(
                                #pname,
                                #server_expr,
                            ).map_err(|_| ipc::Error::ServerDied)?;
                            let #pname: ipc::RawHandle = __dh.handle;
                        })
                    }
                }
                None => None,
            }
        })
        .collect();

    quote! { #(#stmts)* }
}

/// Serialize method arguments into `argbuffer`.
///
/// `handle_expr` — if `Some`, a `RawHandle` expression is prepended to the
/// args tuple (used for instance methods that carry a handle).  Pass `None`
/// for constructors / static messages.
fn gen_serialize_wire(
    wire_names: &[&Ident],
    wire_types: &[TokenStream2],
    handle_expr: Option<&TokenStream2>,
) -> TokenStream2 {
    if let Some(handle) = handle_expr {
        if wire_types.is_empty() {
            quote! {
                let args: (ipc::RawHandle,) = (#handle,);
                let mut argbuffer = [0u8; <(ipc::RawHandle,) as hubpack::SerializedSize>::MAX_SIZE];
                let n = hubpack::serialize(&mut argbuffer, &args).expect("ipc: serialize failed");
            }
        } else {
            quote! {
                const _: () = assert!(
                    <(ipc::RawHandle, #(#wire_types,)*) as hubpack::SerializedSize>::MAX_SIZE
                        <= ipc::HUBRIS_MESSAGE_SIZE_LIMIT,
                    "argument types exceed Hubris message size limit (256 bytes)",
                );
                let args: (ipc::RawHandle, #(#wire_types,)*) = (#handle, #(#wire_names,)*);
                let mut argbuffer = [0u8; <(ipc::RawHandle, #(#wire_types,)*) as hubpack::SerializedSize>::MAX_SIZE];
                let n = hubpack::serialize(&mut argbuffer, &args).expect("ipc: serialize failed");
            }
        }
    } else if wire_types.is_empty() {
        quote! {
            let argbuffer = [];
            let n = 0usize;
        }
    } else {
        quote! {
            const _: () = assert!(
                <(#(#wire_types,)*) as hubpack::SerializedSize>::MAX_SIZE
                    <= ipc::HUBRIS_MESSAGE_SIZE_LIMIT,
                "argument types exceed Hubris message size limit (256 bytes)",
            );
            let args: (#(#wire_types,)*) = (#(#wire_names,)*);
            let mut argbuffer = [0u8; <(#(#wire_types,)*) as hubpack::SerializedSize>::MAX_SIZE];
            let n = hubpack::serialize(&mut argbuffer, &args).expect("ipc: serialize failed");
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

fn gen_send_body(kind: u8, method_id_expr: &TokenStream2, lease_arr: &TokenStream2) -> TokenStream2 {
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);
    quote! {
        #lease_arr
        let argbuffer = &argbuffer[..n];
        let opcode = ipc::opcode(#kind_lit, #method_id_expr);
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
    let map = if map_err {
        quote! { .map_err(|e: ipc::Error| e.upcast()) }
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

// ===========================================================================
// Constructor reply helpers — shared between gen_constructor & reconstruct
// ===========================================================================

/// The deserialization wire type for a constructor reply.
fn ctor_wire_type(ctor_return: &ConstructorReturn) -> TokenStream2 {
    match ctor_return {
        ConstructorReturn::Bare => quote! {
            core::result::Result<ipc::RawHandle, ipc::Error>
        },
        ConstructorReturn::Result(error_type) => quote! {
            core::result::Result<
                core::result::Result<ipc::RawHandle, #error_type>,
                ipc::Error,
            >
        },
        ConstructorReturn::OptionSelf => quote! {
            core::result::Result<
                core::option::Option<ipc::RawHandle>,
                ipc::Error,
            >
        },
    }
}

/// Retbuffer size for a constructor call (concrete error type known).
fn ctor_retbuffer_size(ctor_return: &ConstructorReturn) -> TokenStream2 {
    let wire_type = ctor_wire_type(ctor_return);
    quote! { <#wire_type as hubpack::SerializedSize>::MAX_SIZE }
}

/// Retbuffer size for reconstruction (error type is generic `E`).
/// For `Result` variants, falls back to `HUBRIS_MESSAGE_SIZE_LIMIT` since
/// the concrete error type isn't known at the reconstruction call site.
fn ctor_retbuffer_size_reconstruct(ctor_return: &ConstructorReturn) -> TokenStream2 {
    match ctor_return {
        ConstructorReturn::Result(_) => quote! { ipc::HUBRIS_MESSAGE_SIZE_LIMIT },
        _ => ctor_retbuffer_size(ctor_return),
    }
}

// ===========================================================================
// Constructs helpers — for messages that return handles to other resources
// ===========================================================================

/// Generate the mapping expression that converts the wire value `v`
/// (containing `RawHandle`) into the user-facing type (containing handles).
///
/// For `Option<FS>`: maps `v` from `Option<RawHandle>` to
/// `Option<FileSystemHandle<FS>>` via `v.map(|h| Handle::from_raw(h))`.
fn gen_constructs_map(
    return_type: Option<&syn::Type>,
    generic_ident: &Ident,
    handle_type: &Ident,
) -> TokenStream2 {
    let Some(rt) = return_type else {
        return quote! { v };
    };

    // Check if the outer type is Option<FS> or Result<FS, E>, etc.
    if let syn::Type::Path(p) = rt {
        if let Some(seg) = p.path.segments.last() {
            if seg.ident == "Option" {
                // Option<FS> → v.map(|raw| Handle::from_raw(raw))
                return quote! {
                    v.map(|raw| #handle_type::<#generic_ident>::from_raw(raw))
                };
            }
            if seg.ident == "Result" {
                // Result<FS, E> → v.map(|raw| Handle::from_raw(raw))
                return quote! {
                    v.map(|raw| #handle_type::<#generic_ident>::from_raw(raw))
                };
            }
        }
        // Bare FS → Handle::from_raw(v)
        if p.path.get_ident().map(|i| i == generic_ident).unwrap_or(false) {
            return quote! {
                #handle_type::<#generic_ident>::from_raw(v)
            };
        }
    }

    // Fallback: return as-is (shouldn't happen with well-formed types).
    quote! { v }
}
