mod constructs;
mod dyn_methods;
mod error_types;
mod methods;
mod reply;
mod traits;

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::{CloneMode, MethodKind, ParsedMethod, ResourceAttr};
use crate::util::to_snake_case;

use self::dyn_methods::{gen_dyn_method, gen_dyn_static_method};
use self::methods::gen_client_method;
use self::traits::{gen_cloneable_impl, gen_into_dyn_handle, gen_transferable_impl};

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
    let server_trait_name = format_ident!("{}Server", trait_name);
    let binding_struct_name = format_ident!("{}Server", trait_name);
    let bind_macro_name = format_ident!("bind_{}", to_snake_case(&trait_name.to_string()));

    let peer_guard = {
        let pkg = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
        let task_name = pkg.strip_suffix("_api").unwrap_or(&pkg).to_string();
        quote! {
            ipc::__check_uses!(#task_name);
        }
    };

    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);
    let enum_name = format_ident!("{}Op", trait_name);

    let method_impls: Vec<TokenStream2> = methods
        .iter()
        .map(|m| gen_client_method(m, kind, &enum_name))
        .collect();

    let transferable_impl = gen_transferable_impl(&handle_name, &server_trait_name, kind);

    let cloneable_impl = if attrs.clone_mode == Some(CloneMode::Refcount) {
        gen_cloneable_impl(&handle_name, &server_trait_name, kind)
    } else {
        quote! {}
    };

    let into_dyn_handle_impl = if attrs.implements.is_some() {
        gen_into_dyn_handle(&handle_name, &server_trait_name, kind)
    } else {
        quote! {}
    };

    let mod_name = format_ident!("{}_client", to_snake_case(&trait_name.to_string()));

    quote! {
        pub mod #mod_name {
            use super::*;

            pub trait #server_trait_name {
                fn task_id() -> ipc::kern::TaskId;
                fn server_id() -> &'static ipc::StaticTaskId;
            }

            pub struct #handle_name<S: #server_trait_name> {
                server: core::cell::Cell<ipc::kern::TaskId>,
                handle: core::cell::Cell<ipc::RawHandle>,
                destroyed: core::cell::Cell<bool>,
                _server: core::marker::PhantomData<S>,
            }

            impl<S: #server_trait_name> #handle_name<S> {
                /// Adopt a raw handle (e.g. from a transfer).
                pub fn from_raw(handle: ipc::RawHandle) -> Self {
                    Self {
                        server: core::cell::Cell::new(S::task_id()),
                        handle: core::cell::Cell::new(handle),
                        destroyed: core::cell::Cell::new(false),
                        _server: core::marker::PhantomData,
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

                /// Get the server's TaskId (for use in panic handlers / `notify_dead!`).
                pub fn server_task_id() -> ipc::kern::TaskId {
                    S::task_id()
                }

                #(#method_impls)*
            }

            #transferable_impl
            #cloneable_impl
            #into_dyn_handle_impl

            impl<S: #server_trait_name> Drop for #handle_name<S> {
                fn drop(&mut self) {
                    if self.destroyed.get() {
                        return;
                    }
                    let handle = self.handle.get();
                    let buf = unsafe { &mut *ipc::ipc_buf() };
                    let n = match ipc::__postcard::to_slice(&handle, &mut buf[..]) {
                        Ok(s) => s.len(),
                        Err(_) => return,
                    };
                    let opcode = ipc::opcode(#kind_lit, ipc::IMPLICIT_DESTROY_METHOD);
                    let mut leases = [];
                    let _ = ipc::kern::sys_send(
                        self.server.get(),
                        opcode,
                        buf,
                        n,
                        &mut leases,
                    );
                }
            }
        }

        #[macro_export]
        macro_rules! #bind_macro_name {
            ($name:ident = $slot:expr) => {
                #peer_guard
                #[doc(hidden)]
                pub struct #binding_struct_name;
                impl $crate::#mod_name::#server_trait_name for #binding_struct_name {
                    fn task_id() -> ipc::kern::TaskId { $slot }
                    fn server_id() -> &'static ipc::StaticTaskId {
                        static SERVER_ID: ipc::StaticTaskId = ipc::StaticTaskId::new($slot);
                        &SERVER_ID
                    }
                }
                pub type $name = $crate::#mod_name::#handle_name<#binding_struct_name>;
            };
        }

    pub use #mod_name::*;
    }
}

// ===========================================================================
// Dynamic client (for interface-only traits, no arena)
// ===========================================================================

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
        .map(gen_dyn_method)
        .collect();

    let static_method_impls: Vec<TokenStream2> = methods
        .iter()
        .filter(|m| m.kind == MethodKind::StaticMessage)
        .map(gen_dyn_static_method)
        .collect();

    quote! {
        pub mod #mod_name {
            use super::*;

            /// Dynamic client for any server implementing this interface.
            /// Created from a `DynHandle` received via handle forwarding.
            pub struct #dyn_name {
                server: core::cell::Cell<ipc::kern::TaskId>,
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
                fn transfer_info(&self) -> ipc::DynHandle {
                    ipc::DynHandle {
                        server_id: u16::from(self.server.get()),
                        kind: self.kind,
                        handle: self.handle.get(),
                    }
                }
            }

            impl Drop for #dyn_name {
                fn drop(&mut self) {
                    let handle = self.handle.get();
                    let buf = unsafe { &mut *ipc::ipc_buf() };
                    let n = match ipc::__postcard::to_slice(&handle, &mut buf[..]) {
                        Ok(s) => s.len(),
                        Err(_) => return,
                    };
                    let opcode = ipc::opcode(self.kind, ipc::IMPLICIT_DESTROY_METHOD);
                    let mut leases = [];
                    let _ = ipc::kern::sys_send(
                        self.server.get(),
                        opcode,
                        buf,
                        n,
                        &mut leases,
                    );
                }
            }
        }

        pub use #mod_name::*;
    }
}
