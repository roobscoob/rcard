use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::{parse_slice_ref, MethodKind, ParsedMethod, ParsedParam};
use crate::util::to_pascal_case;

pub fn gen_server_trait(trait_name: &Ident, methods: &[ParsedMethod]) -> TokenStream2 {
    let method_fns: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let name = &m.name;
            let mut params = Vec::new();

            params.push(quote! { meta: ipc::Meta });

            for p in &m.params {
                let pname = &p.name;
                if p.is_lease {
                    if let Some((inner_ty, mutable)) = parse_slice_ref(&p.ty) {
                        if mutable {
                            params.push(
                                quote! { #pname: idyll_runtime::Leased<idyll_runtime::Write, #inner_ty> },
                            );
                        } else {
                            params.push(
                                quote! { #pname: idyll_runtime::Leased<idyll_runtime::Read, #inner_ty> },
                            );
                        }
                    }
                } else {
                    let ty = &p.ty;
                    params.push(quote! { #pname: #ty });
                }
            }

            // Wrap the user's original return type in Result<_, ReplyFaultReason>.
            // -> Self          becomes -> Result<Self, RFR>
            // -> Result<Self, E> becomes -> Result<Result<Self, E>, RFR>
            // -> ()             becomes -> Result<(), RFR>
            let inner = if let Some(rt) = &m.return_type {
                quote! { #rt }
            } else if m.kind == MethodKind::Constructor {
                quote! { Self }
            } else {
                quote! { () }
            };
            let ret = quote! { -> core::result::Result<#inner, userlib::ReplyFaultReason> };

            let receiver = match m.kind {
                MethodKind::Constructor => quote! {},
                MethodKind::Message => quote! { &mut self, },
                MethodKind::Destructor => quote! { self, },
            };

            quote! {
                fn #name(#receiver #(#params),*) #ret;
            }
        })
        .collect();

    quote! {
        pub trait #trait_name: Sized {
            #(#method_fns)*
        }
    }
}

pub fn gen_operation_enum(trait_name: &Ident, methods: &[ParsedMethod]) -> TokenStream2 {
    let enum_name = format_ident!("{}Op", trait_name);

    let variants: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            let id = m.method_id;
            quote! { #variant = #id }
        })
        .collect();

    let match_arms: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            let id = m.method_id;
            quote! { #id => Ok(#enum_name::#variant) }
        })
        .collect();

    quote! {
        #[derive(Copy, Clone, Debug)]
        #[repr(u8)]
        pub enum #enum_name {
            #(#variants),*
        }

        impl TryFrom<u8> for #enum_name {
            type Error = u8;
            fn try_from(x: u8) -> core::result::Result<Self, Self::Error> {
                match x {
                    #(#match_arms,)*
                    other => Err(other),
                }
            }
        }
    }
}

pub fn gen_dispatcher(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    arena_size: usize,
) -> TokenStream2 {
    let dispatcher_name = format_ident!("{}Dispatcher", trait_name);
    let enum_name = format_ident!("{}Op", trait_name);

    let dispatch_arms: Vec<TokenStream2> = methods
        .iter()
        .map(|m| gen_dispatch_arm(trait_name, &enum_name, m))
        .collect();

    quote! {
        pub struct #dispatcher_name<T: #trait_name> {
            pub arena: ipc::Arena<T, #arena_size>,
        }

        impl<T: #trait_name> #dispatcher_name<T> {
            pub const fn new() -> Self {
                Self {
                    arena: ipc::Arena::new(),
                }
            }
        }

        impl<T: #trait_name> ipc::ResourceDispatch for #dispatcher_name<T> {
            fn dispatch(
                &mut self,
                method_id: u8,
                msg: &userlib::Message<'_>,
            ) -> core::result::Result<(), userlib::ReplyFaultReason> {
                // Handle implicit destroy (Drop on client handle).
                if method_id == ipc::IMPLICIT_DESTROY_METHOD {
                    let Ok(msg_data) = &msg.data else {
                        return Err(userlib::ReplyFaultReason::BadMessageSize);
                    };
                    let (handle, _) = hubpack::deserialize::<ipc::RawHandle>(msg_data)
                        .map_err(|_| userlib::ReplyFaultReason::BadMessageContents)?;
                    let _ = self.arena.remove(handle); // Drop runs here
                    userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &[]);
                    return Ok(());
                }

                let op = #enum_name::try_from(method_id)
                    .map_err(|_| userlib::ReplyFaultReason::UndefinedOperation)?;

                let Ok(msg_data) = &msg.data else {
                    return Err(userlib::ReplyFaultReason::BadMessageSize);
                };

                let meta = ipc::Meta {
                    sender: msg.sender,
                    lease_count: msg.lease_count as u8,
                };

                match op {
                    #(#dispatch_arms)*
                }
            }

            fn cleanup_client(&mut self, task_index: u16) {
                self.arena.remove_by_owner(task_index);
            }
        }
    }
}

fn gen_dispatch_arm(
    _trait_name: &Ident,
    enum_name: &Ident,
    m: &ParsedMethod,
) -> TokenStream2 {
    let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));

    let non_lease_params: Vec<&ParsedParam> =
        m.params.iter().filter(|p| !p.is_lease).collect();
    let lease_params: Vec<&ParsedParam> =
        m.params.iter().filter(|p| p.is_lease).collect();

    let lease_bindings = gen_lease_bindings(&lease_params);
    let call_args = gen_call_args(m);

    let method_name = &m.name;

    match m.kind {
        MethodKind::Constructor => {
            let (deserialize, destructure) = gen_deserialize_args(&non_lease_params, false);

            let ctor_body = if m.return_type.is_some() {
                // User specified a return type (e.g. Result<Self, E>).
                // Use ConstructorResult to extract Self or serialize the error.
                // On success: reply with raw handle bytes (matching infallible path).
                // On domain error: reply fault (BadMessageContents) — the client
                // sees this as a failed open.  Proper domain-error propagation
                // can be added later.
                quote! {
                    let ctor_result = T::#method_name(meta, #(#call_args),*)?;
                    match ipc::ConstructorResult::into_resource(ctor_result) {
                        Ok(value) => {
                            let handle = self.arena.alloc(value, msg.sender.task_index())
                                .ok_or(userlib::ReplyFaultReason::BadMessageContents)?;
                            let reply = zerocopy::IntoBytes::as_bytes(&handle);
                            userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, reply);
                        }
                        Err(_e) => {
                            return Err(userlib::ReplyFaultReason::BadMessageContents);
                        }
                    }
                }
            } else {
                // No return type (-> Self) — alloc directly, reply with raw handle.
                quote! {
                    let value = T::#method_name(meta, #(#call_args),*)?;
                    let handle = self.arena.alloc(value)
                        .ok_or(userlib::ReplyFaultReason::BadMessageContents)?;
                    let reply = zerocopy::IntoBytes::as_bytes(&handle);
                    userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, reply);
                }
            };

            quote! {
                #enum_name::#variant => {
                    #deserialize
                    #destructure
                    #(#lease_bindings)*
                    #ctor_body
                    Ok(())
                }
            }
        }

        MethodKind::Message => {
            let (deserialize, destructure) = gen_deserialize_args(&non_lease_params, true);
            let handle_result = gen_reply(method_name, &call_args, m.return_type.as_ref());

            quote! {
                #enum_name::#variant => {
                    #deserialize
                    #destructure
                    #(#lease_bindings)*
                    let resource = self.arena.get_mut(handle)
                        .ok_or(userlib::ReplyFaultReason::BadMessageContents)?;
                    #handle_result
                    Ok(())
                }
            }
        }

        MethodKind::Destructor => {
            let (deserialize, destructure) = gen_deserialize_args(&non_lease_params, true);
            let handle_result = gen_reply(method_name, &call_args, m.return_type.as_ref());

            quote! {
                #enum_name::#variant => {
                    #deserialize
                    #destructure
                    let resource = self.arena.remove(handle)
                        .ok_or(userlib::ReplyFaultReason::BadMessageContents)?;
                    #handle_result
                    Ok(())
                }
            }
        }
    }
}

fn gen_deserialize_args(
    non_lease_params: &[&ParsedParam],
    include_handle: bool,
) -> (TokenStream2, TokenStream2) {
    let arg_types: Vec<_> = non_lease_params.iter().map(|p| &p.ty).collect();
    let arg_names: Vec<_> = non_lease_params.iter().map(|p| &p.name).collect();

    if include_handle {
        let deserialize = if arg_types.is_empty() {
            quote! {
                let (handle, _) = hubpack::deserialize::<ipc::RawHandle>(msg_data)
                    .map_err(|_| userlib::ReplyFaultReason::BadMessageContents)?;
            }
        } else {
            quote! {
                let (args, _) = hubpack::deserialize::<(ipc::RawHandle, #(#arg_types,)*)>(msg_data)
                    .map_err(|_| userlib::ReplyFaultReason::BadMessageContents)?;
            }
        };

        let destructure = if arg_types.is_empty() {
            quote! {}
        } else if arg_names.len() == 1 {
            let n = &arg_names[0];
            quote! { let (handle, #n) = args; }
        } else {
            quote! { let (handle, #(#arg_names,)*) = args; }
        };

        (deserialize, destructure)
    } else {
        let deserialize = if arg_types.is_empty() {
            quote! {}
        } else if arg_names.len() == 1 {
            let ty = &arg_types[0];
            let n = &arg_names[0];
            quote! {
                let (#n, _) = hubpack::deserialize::<#ty>(msg_data)
                    .map_err(|_| userlib::ReplyFaultReason::BadMessageContents)?;
            }
        } else {
            quote! {
                let (args, _) = hubpack::deserialize::<(#(#arg_types,)*)>(msg_data)
                    .map_err(|_| userlib::ReplyFaultReason::BadMessageContents)?;
            }
        };

        let destructure = if arg_names.len() > 1 {
            quote! { let (#(#arg_names,)*) = args; }
        } else {
            quote! {}
        };

        (deserialize, destructure)
    }
}

fn gen_lease_bindings(lease_params: &[&ParsedParam]) -> Vec<TokenStream2> {
    lease_params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let pname = &p.name;
            if p.lease_mutable {
                quote! {
                    let #pname = idyll_runtime::Leased::<idyll_runtime::Write, _>::new(msg.sender, #i)
                        .ok_or(userlib::ReplyFaultReason::BadLeases)?;
                }
            } else {
                quote! {
                    let #pname = idyll_runtime::Leased::<idyll_runtime::Read, _>::new(msg.sender, #i)
                        .ok_or(userlib::ReplyFaultReason::BadLeases)?;
                }
            }
        })
        .collect()
}

fn gen_call_args(m: &ParsedMethod) -> Vec<TokenStream2> {
    m.params
        .iter()
        .map(|p| {
            let n = &p.name;
            quote! { #n }
        })
        .collect()
}

fn gen_reply(
    method_name: &Ident,
    call_args: &[TokenStream2],
    return_type: Option<&syn::Type>,
) -> TokenStream2 {
    if return_type.is_some() {
        quote! {
            let result = resource.#method_name(meta, #(#call_args),*)?;
            let mut reply_buf = [0u8; 64];
            let n = hubpack::serialize(&mut reply_buf, &result).unwrap_or(0);
            userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
        }
    } else {
        quote! {
            resource.#method_name(meta, #(#call_args),*)?;
            userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &[]);
        }
    }
}
