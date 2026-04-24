use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::lease;
use crate::parse::{
    ConstructorReturn, MethodKind, ParsedMethod, ParsedParam, ResourceAttr,
    collect_peer_traits,
};
use crate::transfer;
use crate::util::{panic_path, to_pascal_case};
use crate::wire_format;

use super::peers::{
    is_resolvable_peer, peer_const_name, peer_field_name, peer_generic_name,
};
use super::reply::{gen_reply, gen_static_reply, server_return_type};

pub fn gen_dispatcher(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    attrs: &ResourceAttr,
) -> TokenStream2 {
    let _p = panic_path();
    let arena_size = attrs.arena_size.unwrap_or(0);
    let dispatcher_name = format_ident!("{}Dispatcher", trait_name);
    let enum_name = format_ident!("{}Op", trait_name);
    let peers = collect_peer_traits(methods);

    let dispatch_arms: Vec<TokenStream2> = methods
        .iter()
        .map(|m| gen_dispatch_arm(trait_name, &enum_name, m))
        .collect();

    // Build generic params, fields, constructor args, and trait bounds
    let peer_generic_defs: Vec<TokenStream2> = peers
        .iter()
        .map(|(name, _)| {
            let g = peer_generic_name(name);
            let c = peer_const_name(name);
            quote! { #g, const #c: usize }
        })
        .collect();

    let peer_fields: Vec<TokenStream2> = peers
        .iter()
        .map(|(name, _)| {
            let field = peer_field_name(name);
            let g = peer_generic_name(name);
            let c = peer_const_name(name);
            quote! { #field: &'a ipc::SharedArena<#g, #c> }
        })
        .collect();

    let peer_ctor_params: Vec<TokenStream2> = peers
        .iter()
        .map(|(name, _)| {
            let field = peer_field_name(name);
            let g = peer_generic_name(name);
            let c = peer_const_name(name);
            quote! { #field: &'a ipc::SharedArena<#g, #c> }
        })
        .collect();

    let peer_ctor_inits: Vec<TokenStream2> = peers
        .iter()
        .map(|(name, _)| {
            let field = peer_field_name(name);
            quote! { #field }
        })
        .collect();

    let peer_generic_names: Vec<TokenStream2> = peers
        .iter()
        .map(|(name, _)| {
            let g = peer_generic_name(name);
            let c = peer_const_name(name);
            quote! { #g, #c }
        })
        .collect();

    let trait_bound = if peers.is_empty() {
        quote! { #trait_name }
    } else {
        let peer_gs: Vec<_> = peers
            .iter()
            .map(|(name, _)| peer_generic_name(name))
            .collect();
        quote! { #trait_name<#(#peer_gs),*> }
    };

    quote! {
        pub struct #dispatcher_name<'a, T: #trait_bound, #(#peer_generic_defs),*> {
            arena: &'a ipc::SharedArena<T, #arena_size>,
            priority_fn: fn(u16) -> i8,
            self_task_index: u16,
            #(#peer_fields,)*
        }

        impl<'a, T: #trait_bound, #(#peer_generic_defs),*> #dispatcher_name<'a, T, #(#peer_generic_names),*> {
            pub fn new(
                arena: &'a ipc::SharedArena<T, #arena_size>,
                priority_fn: fn(u16) -> i8,
                self_task_index: u16,
                #(#peer_ctor_params,)*
            ) -> Self {
                Self {
                    arena,
                    priority_fn,
                    self_task_index,
                    #(#peer_ctor_inits,)*
                }
            }
        }

        impl<'a, T: #trait_bound, #(#peer_generic_defs),*> ipc::ResourceDispatch for #dispatcher_name<'a, T, #(#peer_generic_names),*> {
            fn dispatch(
                &mut self,
                method_id: u8,
                msg: ipc::dispatch::MessageData<'_>,
                reply: ipc::dispatch::PendingReply,
            ) {
                let sender_index = msg.sender_index();

                // Handle implicit protocol methods (destroy, clone, 2PC).
                let reply = match self.arena.dispatch_implicit(method_id, &msg, reply, sender_index, self.priority_fn) {
                    Some(reply) => reply,
                    None => return,
                };

                let __priority = (self.priority_fn)(sender_index);

                let op = match #enum_name::try_from(method_id) {
                    Ok(op) => op,
                    Err(_) => #_p!("ipc: unknown method_id"),
                };

                let meta = msg.meta();

                match op {
                    #(#dispatch_arms)*
                }
            }

            fn cleanup_client(&mut self, task_index: u16) {
                self.arena.remove_by_owner(task_index);
                self.arena.cancel_transfers_to(task_index);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-method dispatch arm generation
// ---------------------------------------------------------------------------

fn gen_dispatch_arm(_trait_name: &Ident, enum_name: &Ident, m: &ParsedMethod) -> TokenStream2 {
    let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));

    let non_lease_params: Vec<&ParsedParam> = m.params.iter().filter(|p| !p.is_lease).collect();
    let lease_params: Vec<&ParsedParam> = m.params.iter().filter(|p| p.is_lease).collect();

    let (deserialize, destructure) = match m.kind {
        MethodKind::Constructor | MethodKind::StaticMessage => {
            wire_format::gen_deserialize_args(&non_lease_params, false)
        }
        MethodKind::Message | MethodKind::Destructor => {
            wire_format::gen_deserialize_args(&non_lease_params, true)
        }
    };

    let lease_bindings = lease::gen_lease_bindings(&lease_params);
    let peer_resolution = gen_peer_resolution(m);
    let (acquire_block, _) = transfer::gen_acquire_stmts(m);
    let call_args = gen_call_args(m);

    let method_name = &m.name;

    let body = match m.kind {
        MethodKind::Constructor => {
            let ctor_return = m
                .ctor_return
                .as_ref()
                .expect("constructor must have ctor_return");
            gen_ctor_body(method_name, &call_args, ctor_return)
        }
        MethodKind::Message => {
            let effective_rt = server_return_type(m);
            gen_reply(method_name, &call_args, effective_rt.as_ref(), false)
        }
        MethodKind::Destructor => {
            gen_reply(method_name, &call_args, m.return_type.as_ref(), true)
        }
        MethodKind::StaticMessage => {
            let effective_rt = server_return_type(m);
            gen_static_reply(method_name, &call_args, effective_rt.as_ref())
        }
    };

    quote! {
        #enum_name::#variant => {
            #deserialize
            #destructure
            #(#lease_bindings)*
            #(#peer_resolution)*
            #acquire_block
            #body
        }
    }
}

/// Generate resolution statements for resolvable peer params.
fn gen_peer_resolution(m: &ParsedMethod) -> Vec<TokenStream2> {
    m.params
        .iter()
        .filter_map(|p| {
            if !is_resolvable_peer(p) {
                return None;
            }
            let name = &p.name;
            let trait_name = p.impl_trait_name.as_ref().unwrap();
            let field = peer_field_name(trait_name);
            Some(quote! {
                let Some(#name) = self.#field.get(#name.handle) else {
                    reply.reply_ok(&[1u8, ipc::Error::HandleLost as u8]);
                    return;
                };
            })
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

/// Postcard-encode `value_expr` into `__buf[__off..]`, advancing `__off`.
///
/// Mirrors the Message-reply encoding in `wire_format::gen_encode_return_value`
/// so constructor replies use the same wire format as the schema says
/// (postcard varint for `RawHandle`, not zerocopy fixed-8-bytes-LE).
fn gen_postcard_append(value_expr: TokenStream2) -> TokenStream2 {
    quote! {
        {
            // SAFETY: postcard::to_slice only *writes* to the buffer.
            let __tail: &mut [u8] = unsafe {
                core::slice::from_raw_parts_mut(
                    __buf.as_mut_ptr().add(__off) as *mut u8,
                    __buf.len() - __off,
                )
            };
            match ipc::__postcard::to_slice(&(#value_expr), __tail) {
                Ok(slice) => { __off += slice.len(); }
                Err(_) => {
                    ipc::__ipc_panic!("postcard ctor reply encode failed");
                }
            }
        }
    }
}

fn gen_ctor_body(
    method_name: &Ident,
    call_args: &[TokenStream2],
    ctor_return: &ConstructorReturn,
) -> TokenStream2 {
    let encode_handle = gen_postcard_append(quote! { handle });
    let encode_err = gen_postcard_append(quote! { e });
    match ctor_return {
        ConstructorReturn::Bare => {
            quote! {
                let value = T::#method_name(meta, #(#call_args),*);
                match self.arena.alloc(value, sender_index, __priority) {
                    Ok(handle) => {
                        let mut __buf: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
                            unsafe { core::mem::MaybeUninit::uninit().assume_init() };
                        let mut __off = 0usize;
                        ipc::wire::set_uninit(&mut __buf, __off, 0); // outer Ok
                        __off += 1;
                        #encode_handle
                        reply.reply_ok(unsafe {
                            ipc::wire::assume_init_slice(&__buf, __off)
                        });
                    }
                    Err(_) => {
                        reply.reply_ok(&[1u8, ipc::Error::ArenaFull as u8]);
                    }
                }
            }
        }
        ConstructorReturn::Result(error_type) => {
            let _ = error_type;
            quote! {
                let ctor_result = T::#method_name(meta, #(#call_args),*);
                let mut __buf: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
                    unsafe { core::mem::MaybeUninit::uninit().assume_init() };
                let mut __off = 0usize;
                match ctor_result {
                    Ok(value) => match self.arena.alloc(value, sender_index, __priority) {
                        Ok(handle) => {
                            ipc::wire::set_uninit(&mut __buf, __off, 0); // outer Ok
                            __off += 1;
                            ipc::wire::set_uninit(&mut __buf, __off, 0); // inner Ok tag
                            __off += 1;
                            #encode_handle
                            reply.reply_ok(unsafe { ipc::wire::assume_init_slice(&__buf, __off) });
                        }
                        Err(_) => {
                            reply.reply_ok(&[1u8, ipc::Error::ArenaFull as u8]); // outer Err
                        }
                    },
                    Err(e) => {
                        ipc::wire::set_uninit(&mut __buf, __off, 0); // outer Ok
                        __off += 1;
                        ipc::wire::set_uninit(&mut __buf, __off, 1); // inner Err tag
                        __off += 1;
                        #encode_err
                        reply.reply_ok(unsafe { ipc::wire::assume_init_slice(&__buf, __off) });
                    }
                }
            }
        }
        ConstructorReturn::OptionSelf => {
            quote! {
                let ctor_result = T::#method_name(meta, #(#call_args),*);
                match ctor_result {
                    Some(value) => match self.arena.alloc(value, sender_index, __priority) {
                        Ok(handle) => {
                            let mut __buf: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
                                unsafe { core::mem::MaybeUninit::uninit().assume_init() };
                            let mut __off = 0usize;
                            ipc::wire::set_uninit(&mut __buf, __off, 0); // outer Ok
                            __off += 1;
                            ipc::wire::set_uninit(&mut __buf, __off, 0); // Some tag
                            __off += 1;
                            #encode_handle
                            reply.reply_ok(unsafe {
                                ipc::wire::assume_init_slice(&__buf, __off)
                            });
                        }
                        Err(_) => {
                            reply.reply_ok(&[1u8, ipc::Error::ArenaFull as u8]); // outer Err
                        }
                    },
                    None => {
                        reply.reply_ok(&[0u8, 1u8]); // Ok(None)
                    }
                }
            }
        }
    }
}
