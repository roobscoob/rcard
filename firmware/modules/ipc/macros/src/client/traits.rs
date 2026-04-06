use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::Ident;

use crate::util::panic_path;

/// Generate `impl Transferable` for a concrete handle type.
pub fn gen_transferable_impl(
    handle_name: &Ident,
    server_trait_name: &Ident,
    kind: u8,
) -> TokenStream2 {
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);

    quote! {
        impl<S: #server_trait_name> ipc::Transferable for #handle_name<S> {
            fn transfer_info(&self) -> ipc::DynHandle {
                ipc::DynHandle {
                    server_id: u16::from(self.server.get()),
                    kind: #kind_lit,
                    handle: self.handle.get(),
                }
            }
        }
    }
}

/// Generate `impl Cloneable` for a refcounted handle type.
pub fn gen_cloneable_impl(
    handle_name: &Ident,
    server_trait_name: &Ident,
    kind: u8,
) -> TokenStream2 {
    let _p = panic_path();
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);

    quote! {
        impl<S: #server_trait_name> ipc::Cloneable for #handle_name<S> {
            fn clone_for(&self, new_owner: ipc::kern::TaskId) -> core::result::Result<ipc::DynHandle, ipc::CloneError> {
                let mut argbuffer = [0u8; ipc::RawHandle::SIZE + core::mem::size_of::<u16>()];
                let mut n = 0usize;
                n += ipc::wire::write(&mut argbuffer[n..], &self.handle.get());
                let owner_idx = new_owner.task_index();
                n += ipc::wire::write(&mut argbuffer[n..], &owner_idx);
                let mut __retbuf_mem: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
                    unsafe { core::mem::MaybeUninit::uninit().assume_init() };
                let retbuffer = unsafe { ipc::wire::as_mut_byte_slice(&mut __retbuf_mem) };
                let mut leases = [];
                let opcode = ipc::opcode(#kind_lit, ipc::CLONE_METHOD);
                let (rc, len) = ipc::kern::sys_send(
                    self.server.get(),
                    opcode,
                    &argbuffer[..n],
                    retbuffer,
                    &mut leases,
                ).map_err(|_| ipc::CloneError::ServerDied)?;
                if rc == ipc::ACCESS_VIOLATION {
                    #_p!("ipc: clone rejected: access violation \
                           (this task is not authorized to use this server)");
                }
                if rc != ipc::kern::ResponseCode::SUCCESS {
                    #_p!("ipc: clone got non-SUCCESS response code");
                }
                // Wire format: tag(0=Ok,1=Err) + payload
                if len == 0 { #_p!("ipc: empty clone reply"); }
                let new_handle = match retbuffer[0] {
                    0u8 => {
                        let Some((h, _)) = ipc::wire::read::<ipc::RawHandle>(&retbuffer[1..len]) else {
                            #_p!("ipc: malformed clone reply");
                        };
                        h
                    }
                    1u8 => {
                        let Some((err, _)) = ipc::wire::read::<ipc::Error>(&retbuffer[1..len]) else {
                            #_p!("ipc: malformed clone error reply");
                        };
                        match err {
                            ipc::Error::HandleLost => return Err(ipc::CloneError::InvalidHandle),
                            ipc::Error::ArenaFull => return Err(ipc::CloneError::ArenaFull),
                            _ => return Err(ipc::CloneError::ServerDied),
                        }
                    }
                    _ => #_p!("ipc: invalid clone reply tag"),
                };
                Ok(ipc::DynHandle {
                    server_id: u16::from(self.server.get()),
                    kind: #kind_lit,
                    handle: new_handle,
                })
            }
        }
    }
}

/// Generate `impl From<Handle<S>> for DynHandle` when a resource implements an interface.
pub fn gen_into_dyn_handle(
    handle_name: &Ident,
    server_trait_name: &Ident,
    kind: u8,
) -> TokenStream2 {
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);
    quote! {
        impl<S: #server_trait_name> From<#handle_name<S>> for ipc::DynHandle {
            fn from(h: #handle_name<S>) -> ipc::DynHandle {
                let dh = ipc::DynHandle {
                    server_id: u16::from(h.server.get()),
                    kind: #kind_lit,
                    handle: h.handle.get(),
                };
                // Prevent Drop from sending destroy — caller now owns the handle.
                core::mem::forget(h);
                dh
            }
        }
    }
}
