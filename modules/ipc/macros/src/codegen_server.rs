use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::{
    parse_slice_ref, ConstructorReturn, MethodKind, ParsedMethod, ParsedParam, ResourceAttr,
};
use crate::util::{replace_ident_in_type, to_pascal_case};

pub fn gen_server_trait(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    _attrs: &ResourceAttr,
) -> TokenStream2 {
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
                } else if p.handle_mode.is_some() {
                    // Handle params become RawHandle (concrete) or DynHandle (impl Trait)
                    // on the server side.
                    if p.impl_trait_name.is_some() {
                        params.push(quote! { #pname: ipc::DynHandle });
                    } else {
                        params.push(quote! { #pname: ipc::RawHandle });
                    }
                } else {
                    let ty = &p.ty;
                    params.push(quote! { #pname: #ty });
                }
            }

            // Wrap the user's original return type in Result<_, ReplyFaultReason>.
            // For constructs messages, replace the generic ident (e.g. `FS`) with `ipc::RawHandle`.
            let inner = if let Some(rt) = &m.return_type {
                if let Some((_trait_name, generic_ident)) = &m.constructs {
                    let replaced = replace_ident_in_type(rt, generic_ident, &quote! { ipc::RawHandle });
                    quote! { #replaced }
                } else {
                    quote! { #rt }
                }
            } else if m.kind == MethodKind::Constructor {
                quote! { Self }
            } else {
                quote! { () }
            };
            let ret = quote! { -> #inner };

            let receiver = match m.kind {
                MethodKind::Constructor | MethodKind::StaticMessage => quote! {},
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

pub fn gen_operation_enum(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    attrs: &ResourceAttr,
) -> TokenStream2 {
    let enum_name = format_ident!("{}Op", trait_name);
    let method_count = methods.len() as u8;
    let method_count_lit = proc_macro2::Literal::u8_suffixed(method_count);

    // When `implements(Trait)` is set, message method discriminants reference
    // the interface's Op enum, and non-message methods start at METHOD_COUNT.
    let iface_op = attrs
        .implements
        .as_ref()
        .map(|p| crate::parse::interface_op_path(p));

    let mut non_message_offset: u8 = 0;

    let variants: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            if let Some(ref iface_op) = iface_op {
                if m.kind == MethodKind::Message || m.kind == MethodKind::StaticMessage {
                    // Reference the interface's Op variant directly.
                    quote! { #variant = #iface_op::#variant as u8 }
                } else {
                    let offset = proc_macro2::Literal::u8_suffixed(non_message_offset);
                    non_message_offset += 1;
                    quote! { #variant = #iface_op::METHOD_COUNT + #offset }
                }
            } else {
                let id = m.method_id;
                quote! { #variant = #id }
            }
        })
        .collect();

    // For TryFrom, use const bindings so patterns work with computed discriminants.
    let const_bindings: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            let const_name = format_ident!("__{}", m.name.to_string().to_uppercase());
            quote! { const #const_name: u8 = #enum_name::#variant as u8; }
        })
        .collect();

    let match_arms: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            let const_name = format_ident!("__{}", m.name.to_string().to_uppercase());
            quote! { #const_name => Ok(#enum_name::#variant) }
        })
        .collect();

    quote! {
        #[derive(Copy, Clone, Debug)]
        #[repr(u8)]
        pub enum #enum_name {
            #(#variants),*
        }

        impl #enum_name {
            pub const METHOD_COUNT: u8 = #method_count_lit;
        }

        impl TryFrom<u8> for #enum_name {
            type Error = u8;
            fn try_from(x: u8) -> core::result::Result<Self, Self::Error> {
                #(#const_bindings)*
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
    attrs: &ResourceAttr,
) -> TokenStream2 {
    let arena_size = attrs.arena_size.unwrap_or(0);
    let dispatcher_name = format_ident!("{}Dispatcher", trait_name);
    let enum_name = format_ident!("{}Op", trait_name);

    let dispatch_arms: Vec<TokenStream2> = methods
        .iter()
        .map(|m| gen_dispatch_arm(trait_name, &enum_name, m))
        .collect();

    let kind_lit = proc_macro2::Literal::u8_suffixed(attrs.kind);

    quote! {
        pub struct #dispatcher_name<T: #trait_name> {
            pub arena: ipc::Arena<T, #arena_size>,
        }

        impl<T: #trait_name> #dispatcher_name<T> {
            pub const fn new() -> Self {
                Self {
                    arena: ipc::Arena::new(#kind_lit),
                }
            }
        }

        impl<T: #trait_name> ipc::ResourceDispatch for #dispatcher_name<T> {
            fn dispatch(
                &mut self,
                method_id: u8,
                msg: &userlib::Message<'_>,
            ) -> core::result::Result<(), userlib::ReplyFaultReason> {
                let sender_index = msg.sender.task_index();

                // Handle implicit destroy (Drop on client handle).
                if method_id == ipc::IMPLICIT_DESTROY_METHOD {
                    let Ok(msg_data) = &msg.data else {
                        return Err(userlib::ReplyFaultReason::BadMessageSize);
                    };
                    let (handle, _) = hubpack::deserialize::<ipc::RawHandle>(msg_data)
                        .map_err(|_| userlib::ReplyFaultReason::BadMessageContents)?;
                    let _ = self.arena.remove_owned(handle, sender_index);
                    userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &[]);
                    return Ok(());
                }

                // Handle ownership transfer.
                if method_id == ipc::TRANSFER_METHOD {
                    let Ok(msg_data) = &msg.data else {
                        return Err(userlib::ReplyFaultReason::BadMessageSize);
                    };
                    let (args, _) = hubpack::deserialize::<(ipc::RawHandle, u16)>(msg_data)
                        .map_err(|_| userlib::ReplyFaultReason::BadMessageContents)?;
                    let (handle, new_owner) = args;
                    let ok = self.arena.transfer(handle, sender_index, new_owner);
                    let result: core::result::Result<(), ipc::Error> = if ok {
                        Ok(())
                    } else {
                        Err(ipc::Error::InvalidHandle)
                    };
                    let mut reply_buf = [0u8;
                        <core::result::Result<(), ipc::Error> as hubpack::SerializedSize>::MAX_SIZE];
                    let n = hubpack::serialize(&mut reply_buf, &result)
                        .expect("ipc: failed to serialize transfer reply");
                    userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
                    return Ok(());
                }

                // Handle clone (refcounted resources).
                if method_id == ipc::CLONE_METHOD {
                    let Ok(msg_data) = &msg.data else {
                        return Err(userlib::ReplyFaultReason::BadMessageSize);
                    };
                    let (args, _) = hubpack::deserialize::<(ipc::RawHandle, u16)>(msg_data)
                        .map_err(|_| userlib::ReplyFaultReason::BadMessageContents)?;
                    let (handle, new_owner) = args;
                    let result: core::result::Result<ipc::RawHandle, ipc::Error> =
                        match self.arena.clone_handle(handle, sender_index, new_owner) {
                            Ok(new_handle) => Ok(new_handle),
                            Err(ipc::CloneError::InvalidHandle) => Err(ipc::Error::InvalidHandle),
                            Err(ipc::CloneError::ArenaFull) => Err(ipc::Error::ArenaFull),
                        };
                    let mut reply_buf = [0u8;
                        <core::result::Result<ipc::RawHandle, ipc::Error>
                            as hubpack::SerializedSize>::MAX_SIZE];
                    let n = hubpack::serialize(&mut reply_buf, &result)
                        .expect("ipc: failed to serialize clone reply");
                    userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
                    return Ok(());
                }

                let op = match #enum_name::try_from(method_id) {
                    Ok(op) => op,
                    Err(bad) => panic!(
                        "ipc: unknown method_id {} for {}",
                        bad,
                        stringify!(#enum_name),
                    ),
                };

                let Ok(msg_data) = &msg.data else {
                    return Err(userlib::ReplyFaultReason::BadMessageSize);
                };

                let meta = ipc::Meta {
                    sender: msg.sender,
                    lease_count: u8::try_from(msg.lease_count)
                        .expect("ipc: lease_count exceeds u8"),
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

/// Compute the effective return type for a method's server-side serialization.
/// For `constructs` messages, replaces the generic ident (e.g. `FS`) with `ipc::RawHandle`.
fn server_return_type(m: &ParsedMethod) -> Option<syn::Type> {
    let rt = m.return_type.as_ref()?;
    if let Some((_trait_name, generic_ident)) = &m.constructs {
        let replaced = replace_ident_in_type(rt, generic_ident, &quote! { ipc::RawHandle });
        Some(syn::parse2(replaced).expect("ipc: failed to parse replaced return type"))
    } else {
        Some(rt.clone())
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

            let ctor_return = m.ctor_return.as_ref().expect("constructor must have ctor_return");
            let ctor_body = match ctor_return {
                ConstructorReturn::Bare => {
                    quote! {
                        let value = T::#method_name(meta, #(#call_args),*);
                        let result: core::result::Result<ipc::RawHandle, ipc::Error> =
                            match self.arena.alloc(value, sender_index) {
                                Some(handle) => Ok(handle),
                                None => Err(ipc::Error::ArenaFull),
                            };
                        let mut reply_buf = [0u8;
                            <core::result::Result<ipc::RawHandle, ipc::Error>
                                as hubpack::SerializedSize>::MAX_SIZE];
                        let n = hubpack::serialize(&mut reply_buf, &result)
                            .expect("ipc: failed to serialize constructor reply");
                        userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
                    }
                }
                ConstructorReturn::Result(error_type) => {
                    quote! {
                        let ctor_result = T::#method_name(meta, #(#call_args),*);
                        let result: core::result::Result<
                            core::result::Result<ipc::RawHandle, #error_type>,
                            ipc::Error,
                        > = match ctor_result {
                            Ok(value) => match self.arena.alloc(value, sender_index) {
                                Some(handle) => Ok(Ok(handle)),
                                None => Err(ipc::Error::ArenaFull),
                            },
                            Err(e) => Ok(Err(e)),
                        };
                        let mut reply_buf = [0u8;
                            <core::result::Result<
                                core::result::Result<ipc::RawHandle, #error_type>,
                                ipc::Error,
                            > as hubpack::SerializedSize>::MAX_SIZE];
                        let n = hubpack::serialize(&mut reply_buf, &result)
                            .expect("ipc: failed to serialize constructor reply");
                        userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
                    }
                }
                ConstructorReturn::OptionSelf => {
                    quote! {
                        let ctor_result = T::#method_name(meta, #(#call_args),*);
                        let result: core::result::Result<
                            core::option::Option<ipc::RawHandle>,
                            ipc::Error,
                        > = match ctor_result {
                            Some(value) => match self.arena.alloc(value, sender_index) {
                                Some(handle) => Ok(Some(handle)),
                                None => Err(ipc::Error::ArenaFull),
                            },
                            None => Ok(None),
                        };
                        let mut reply_buf = [0u8;
                            <core::result::Result<
                                core::option::Option<ipc::RawHandle>,
                                ipc::Error,
                            > as hubpack::SerializedSize>::MAX_SIZE];
                        let n = hubpack::serialize(&mut reply_buf, &result)
                            .expect("ipc: failed to serialize constructor reply");
                        userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
                    }
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
            // For constructs messages, the server returns RawHandle, not the generic ident.
            let effective_rt = server_return_type(m);
            let handle_result = gen_reply(method_name, &call_args, effective_rt.as_ref(), false);

            quote! {
                #enum_name::#variant => {
                    #deserialize
                    #destructure
                    #(#lease_bindings)*
                    #handle_result
                    Ok(())
                }
            }
        }

        MethodKind::Destructor => {
            let (deserialize, destructure) = gen_deserialize_args(&non_lease_params, true);
            let handle_result = gen_reply(method_name, &call_args, m.return_type.as_ref(), true);

            quote! {
                #enum_name::#variant => {
                    #deserialize
                    #destructure
                    #handle_result
                    Ok(())
                }
            }
        }

        MethodKind::StaticMessage => {
            let (deserialize, destructure) = gen_deserialize_args(&non_lease_params, false);
            let effective_rt = server_return_type(m);
            let static_reply = gen_static_reply(method_name, &call_args, effective_rt.as_ref());

            quote! {
                #enum_name::#variant => {
                    #deserialize
                    #destructure
                    #(#lease_bindings)*
                    #static_reply
                    Ok(())
                }
            }
        }
    }
}

/// Determine the wire type for a parameter (what gets deserialized).
fn wire_type(p: &ParsedParam) -> TokenStream2 {
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

fn gen_deserialize_args(
    non_lease_params: &[&ParsedParam],
    include_handle: bool,
) -> (TokenStream2, TokenStream2) {
    let arg_types: Vec<TokenStream2> = non_lease_params.iter().map(|p| wire_type(p)).collect();
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

fn gen_static_reply(
    method_name: &Ident,
    call_args: &[TokenStream2],
    return_type: Option<&syn::Type>,
) -> TokenStream2 {
    if let Some(rt) = return_type {
        quote! {
            const _: () = assert!(
                <core::result::Result<#rt, ipc::Error> as hubpack::SerializedSize>::MAX_SIZE
                    <= ipc::HUBRIS_MESSAGE_SIZE_LIMIT,
                "return type exceeds Hubris message size limit (256 bytes)",
            );
            let result_value = T::#method_name(meta, #(#call_args),*);
            let mut reply_buf = [0u8;
                <core::result::Result<#rt, ipc::Error> as hubpack::SerializedSize>::MAX_SIZE];
            let n = hubpack::serialize(
                &mut reply_buf,
                &core::result::Result::<#rt, ipc::Error>::Ok(result_value),
            ).expect("ipc: failed to serialize reply");
            userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
        }
    } else {
        quote! {
            T::#method_name(meta, #(#call_args),*);
            let mut reply_buf =
                [0u8; <core::result::Result<(), ipc::Error> as hubpack::SerializedSize>::MAX_SIZE];
            let n = hubpack::serialize(
                &mut reply_buf,
                &core::result::Result::<(), ipc::Error>::Ok(()),
            ).expect("ipc: failed to serialize reply");
            userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
        }
    }
}

fn gen_reply(
    method_name: &Ident,
    call_args: &[TokenStream2],
    return_type: Option<&syn::Type>,
    is_destructor: bool,
) -> TokenStream2 {
    let arena_op = if is_destructor {
        quote! { self.arena.remove_owned(handle, sender_index) }
    } else {
        quote! { self.arena.get_mut_owned(handle, sender_index) }
    };

    if let Some(rt) = return_type {
        quote! {
            const _: () = assert!(
                <core::result::Result<#rt, ipc::Error> as hubpack::SerializedSize>::MAX_SIZE
                    <= ipc::HUBRIS_MESSAGE_SIZE_LIMIT,
                "return type exceeds Hubris message size limit (256 bytes)",
            );
            let mut reply_buf = [0u8;
                <core::result::Result<#rt, ipc::Error> as hubpack::SerializedSize>::MAX_SIZE];
            let Some(resource) = #arena_op else {
                let n = hubpack::serialize(
                    &mut reply_buf,
                    &core::result::Result::<#rt, ipc::Error>::Err(ipc::Error::InvalidHandle),
                ).expect("ipc: failed to serialize reply");
                userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
                return Ok(());
            };
            let result_value = resource.#method_name(meta, #(#call_args),*);
            let n = hubpack::serialize(
                &mut reply_buf,
                &core::result::Result::<#rt, ipc::Error>::Ok(result_value),
            ).expect("ipc: failed to serialize reply");
            userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
        }
    } else {
        quote! {
            let mut reply_buf =
                [0u8; <core::result::Result<(), ipc::Error> as hubpack::SerializedSize>::MAX_SIZE];
            let Some(resource) = #arena_op else {
                let n = hubpack::serialize(
                    &mut reply_buf,
                    &core::result::Result::<(), ipc::Error>::Err(ipc::Error::InvalidHandle),
                ).expect("ipc: failed to serialize reply");
                userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
                return Ok(());
            };
            resource.#method_name(meta, #(#call_args),*);
            let n = hubpack::serialize(
                &mut reply_buf,
                &core::result::Result::<(), ipc::Error>::Ok(()),
            ).expect("ipc: failed to serialize reply");
            userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &reply_buf[..n]);
        }
    }
}
