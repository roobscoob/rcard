use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::{
    ConstructorReturn, HandleMode, MethodKind, ParsedMethod, ParsedParam, ResourceAttr,
    collect_peer_traits, interface_op_path, parse_slice_ref,
};
use crate::util::{
    panic_path, replace_ident_in_type, to_pascal_case, to_screaming_snake_case, to_snake_case,
};

/// Build a map from impl_trait_name -> PeerGenericName for resolvable peers.
fn peer_generic_name(trait_name: &Ident) -> Ident {
    format_ident!("Peer{}", trait_name)
}

fn peer_field_name(trait_name: &Ident) -> Ident {
    format_ident!("peer_{}", to_snake_case(&trait_name.to_string()))
}

fn peer_const_name(trait_name: &Ident) -> Ident {
    format_ident!(
        "__PEER_{}_N",
        to_screaming_snake_case(&trait_name.to_string())
    )
}

/// Check if a parameter is a resolvable peer (clone + impl Trait).
fn is_resolvable_peer(p: &ParsedParam) -> bool {
    p.handle_mode == Some(HandleMode::Clone) && p.impl_trait_name.is_some()
}

pub fn gen_server_trait(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    _attrs: &ResourceAttr,
) -> TokenStream2 {
    let peers = collect_peer_traits(methods);

    // Generic type params for the trait: <PeerFileSystem, PeerFolder, ...>
    let peer_generics: Vec<TokenStream2> = peers
        .iter()
        .map(|(name, _)| {
            let g = peer_generic_name(name);
            quote! { #g }
        })
        .collect();

    let method_fns: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let name = &m.name;
            let mut params = Vec::new();

            params.push(quote! { meta: ipc::Meta });

            for p in &m.params {
                let pname = &p.name;
                if p.is_lease {
                    if let Some((_inner_ty, mutable)) = parse_slice_ref(&p.ty) {
                        if mutable {
                            params.push(
                                quote! { #pname: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write> },
                            );
                        } else {
                            params.push(
                                quote! { #pname: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read> },
                            );
                        }
                    }
                } else if is_resolvable_peer(p) {
                    // Resolvable peer: becomes &PeerGeneric
                    let peer_g = peer_generic_name(p.impl_trait_name.as_ref().unwrap());
                    params.push(quote! { #pname: &#peer_g });
                } else if p.handle_mode.is_some() {
                    // Non-resolvable handle: DynHandle (impl Trait + move) or RawHandle (concrete)
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

            let inner = if let Some(rt) = &m.return_type {
                if let Some((_trait_name, generic_ident)) = &m.constructs {
                    let replaced =
                        replace_ident_in_type(rt, generic_ident, &quote! { ipc::RawHandle });
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

    if peer_generics.is_empty() {
        quote! {
            pub trait #trait_name: Sized {
                #(#method_fns)*
            }
        }
    } else {
        quote! {
            pub trait #trait_name<#(#peer_generics),*>: Sized {
                #(#method_fns)*
            }
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

    let iface_op = attrs.implements.as_ref().map(interface_op_path);

    let mut non_message_offset: u8 = 0;

    let variants: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            if let Some(ref iface_op) = iface_op {
                if m.kind == MethodKind::Message || m.kind == MethodKind::StaticMessage {
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

    // Trait bound: T: TraitName or T: TraitName<PeerA, PeerB>
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

pub fn gen_constants(trait_name: &Ident, attrs: &ResourceAttr) -> TokenStream2 {
    let arena_size = attrs.arena_size.unwrap_or(0);
    let kind = attrs.kind;
    let screaming = to_screaming_snake_case(&trait_name.to_string());
    let kind_name = format_ident!("{}_KIND", screaming);
    let arena_size_name = format_ident!("{}_ARENA_SIZE", screaming);
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);

    quote! {
        pub const #kind_name: u8 = #kind_lit;
        pub const #arena_size_name: usize = #arena_size;
    }
}

pub fn gen_wiring_macro(trait_name: &Ident, methods: &[ParsedMethod]) -> TokenStream2 {
    let peers = collect_peer_traits(methods);
    let dispatcher_name = format_ident!("{}Dispatcher", trait_name);
    let macro_name = format_ident!("__new_{}Dispatcher", trait_name);

    if peers.is_empty() {
        quote! {
            #[doc(hidden)]
            #[macro_export]
            macro_rules! #macro_name {
                ($own_arena:expr, $priority_fn:expr, $self_task_index:expr; $($all_name:ident => $all_arena:expr),* $(,)?) => {
                    $crate::#dispatcher_name::new($own_arena, $priority_fn, $self_task_index)
                };
            }
        }
    } else {
        let find_calls: Vec<TokenStream2> = peers
            .iter()
            .map(|(name, _)| {
                quote! { __find!(#name) }
            })
            .collect();

        quote! {
            #[doc(hidden)]
            #[macro_export]
            macro_rules! #macro_name {
                ($own_arena:expr, $priority_fn:expr, $self_task_index:expr; $($all_name:ident => $all_arena:expr),* $(,)?) => {{
                    macro_rules! __find {
                        $( ($all_name) => { $all_arena }; )*
                    }
                    $crate::#dispatcher_name::new($own_arena, $priority_fn, $self_task_index, #(#find_calls),*)
                }};
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Acquire logic for #[handle(move)] params during dispatch
// ---------------------------------------------------------------------------

/// Generate acquire statements for move-handle params.
/// For same-server handles (RawHandle), acquires directly from the arena.
/// For cross-server handles (DynHandle / impl Trait), sends ACQUIRE_METHOD IPC.
/// Returns (acquire_stmts, rollback_stmts) token streams.
fn gen_acquire_stmts(m: &ParsedMethod) -> (TokenStream2, TokenStream2) {
    let move_params: Vec<&ParsedParam> = m
        .params
        .iter()
        .filter(|p| p.handle_mode == Some(HandleMode::Move))
        .collect();

    if move_params.is_empty() {
        return (quote! {}, quote! {});
    }

    let mut acquire_stmts = Vec::new();
    let mut acquired_names = Vec::new();

    for (i, p) in move_params.iter().enumerate() {
        let pname = &p.name;
        let acquired_flag = quote::format_ident!("__acquired_{}", pname);

        if p.impl_trait_name.is_some() {
            // Cross-server: send ACQUIRE_METHOD IPC to source server
            acquire_stmts.push(quote! {
                let #acquired_flag = {
                    let mut __acq_call = ipc::call::IpcCall::new(
                        #pname.task_id(), #pname.kind, ipc::ACQUIRE_METHOD,
                    );
                    let __h = #pname.handle;
                    __acq_call.push_arg(&__h);
                    match __acq_call.send_raw() {
                        Ok((rc, len, retbuf)) => {
                            rc == ipc::kern::ResponseCode::SUCCESS
                                && len > 0
                                && retbuf[0] == 0u8
                        }
                        _ => false,
                    }
                };
            });
        } else {
            // Same-server: acquire directly from our own arena
            acquire_stmts.push(quote! {
                let #acquired_flag = self.arena.acquire(#pname, self.self_task_index, __priority);
            });
        }

        acquired_names.push((pname.clone(), acquired_flag, i));
    }

    // Build the combined acquire block with rollback on failure
    let all_acquire = quote! { #(#acquire_stmts)* };

    // Check all flags; if any failed, release all previously-acquired handles
    // and reply with an error.
    let flag_checks: Vec<TokenStream2> = acquired_names
        .iter()
        .enumerate()
        .map(|(check_idx, (_pname, flag, _idx))| {
            // Build rollback stmts for all handles acquired *before* this one
            let rollback_stmts: Vec<TokenStream2> = acquired_names[..check_idx]
                .iter()
                .map(|(prev_name, prev_flag, _)| {
                    let prev_p = move_params.iter().find(|p| &p.name == prev_name).unwrap();
                    if prev_p.impl_trait_name.is_some() {
                        // Cross-server: we acquired ownership, so destroy it on the source
                        quote! {
                            if #prev_flag {
                                let mut __destroy_call = ipc::call::IpcCall::new(
                                    #prev_name.task_id(), #prev_name.kind, ipc::IMPLICIT_DESTROY_METHOD,
                                );
                                let __h = #prev_name.handle;
                                __destroy_call.push_arg(&__h);
                                let _ = __destroy_call.send_void();
                            }
                        }
                    } else {
                        quote! {
                            if #prev_flag {
                                let _ = self.arena.remove_owned(#prev_name, self.self_task_index);
                            }
                        }
                    }
                })
                .collect();
            quote! {
                if !#flag {
                    // Release all previously acquired handles
                    #(#rollback_stmts)*
                    reply.reply_ok(&[1u8, ipc::Error::TransferFailed as u8]);
                    return;
                }
            }
        })
        .collect();

    let acquire_block = quote! {
        #all_acquire
        #(#flag_checks)*
    };

    // Rollback is empty since we handle it inline above
    (acquire_block, quote! {})
}

// ---------------------------------------------------------------------------
// Dispatch arm generation (mostly unchanged, with peer resolution added)
// ---------------------------------------------------------------------------

/// Compute the effective return type for a method's server-side serialization.
fn server_return_type(m: &ParsedMethod) -> Option<syn::Type> {
    let rt = m.return_type.as_ref()?;
    if let Some((_trait_name, generic_ident)) = &m.constructs {
        let replaced = replace_ident_in_type(rt, generic_ident, &quote! { ipc::RawHandle });
        Some(syn::parse2(replaced).expect("ipc: failed to parse replaced return type"))
    } else {
        Some(rt.clone())
    }
}

/// Generate resolution statements for resolvable peer params.
/// After deserialization, each DynHandle param is shadowed with the resolved &PeerType.
/// If resolution fails (handle not found), replies with HandleLost and returns early.
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

fn gen_dispatch_arm(_trait_name: &Ident, enum_name: &Ident, m: &ParsedMethod) -> TokenStream2 {
    let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));

    let non_lease_params: Vec<&ParsedParam> = m.params.iter().filter(|p| !p.is_lease).collect();
    let lease_params: Vec<&ParsedParam> = m.params.iter().filter(|p| p.is_lease).collect();

    let lease_bindings = gen_lease_bindings(&lease_params);
    let peer_resolution = gen_peer_resolution(m);
    let (acquire_block, _) = gen_acquire_stmts(m);
    let call_args = gen_call_args(m);

    let method_name = &m.name;

    match m.kind {
        MethodKind::Constructor => {
            let (deserialize, destructure) = gen_deserialize_args(&non_lease_params, false);

            let ctor_return = m
                .ctor_return
                .as_ref()
                .expect("constructor must have ctor_return");
            let ctor_body = match ctor_return {
                ConstructorReturn::Bare => {
                    quote! {
                        let value = T::#method_name(meta, #(#call_args),*);
                        match self.arena.alloc(value, sender_index, __priority) {
                            Ok(handle) => {
                                let mut __buf: [core::mem::MaybeUninit<u8>; 1 + ipc::RawHandle::SIZE] =
                                    unsafe { core::mem::MaybeUninit::uninit().assume_init() };
                                ipc::wire::set_uninit(&mut __buf, 0, 0); // Ok
                                ipc::wire::write_uninit(&mut __buf[1..], &handle);
                                reply.reply_ok(unsafe {
                                    ipc::wire::assume_init_slice(&__buf, 1 + ipc::RawHandle::SIZE)
                                });
                            }
                            Err(_) => {
                                reply.reply_ok(&[1u8, ipc::Error::ArenaFull as u8]); // Err
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
                        match ctor_result {
                            Ok(value) => match self.arena.alloc(value, sender_index, __priority) {
                                Ok(handle) => {
                                    ipc::wire::set_uninit(&mut __buf, 0, 0); // outer Ok
                                    ipc::wire::set_uninit(&mut __buf, 1, 0); // inner Ok
                                    let __n = ipc::wire::write_uninit(&mut __buf[2..], &handle);
                                    reply.reply_ok(unsafe { ipc::wire::assume_init_slice(&__buf, 2 + __n) });
                                }
                                Err(_) => {
                                    reply.reply_ok(&[1u8, ipc::Error::ArenaFull as u8]); // outer Err
                                }
                            },
                            Err(e) => {
                                ipc::wire::set_uninit(&mut __buf, 0, 0); // outer Ok
                                ipc::wire::set_uninit(&mut __buf, 1, 1); // inner Err
                                let __n = ipc::wire::write_uninit(&mut __buf[2..], &e);
                                reply.reply_ok(unsafe { ipc::wire::assume_init_slice(&__buf, 2 + __n) });
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
                                    let mut __buf: [core::mem::MaybeUninit<u8>; 2 + ipc::RawHandle::SIZE] =
                                        unsafe { core::mem::MaybeUninit::uninit().assume_init() };
                                    ipc::wire::set_uninit(&mut __buf, 0, 0); // outer Ok
                                    ipc::wire::set_uninit(&mut __buf, 1, 0); // Some
                                    ipc::wire::write_uninit(&mut __buf[2..], &handle);
                                    reply.reply_ok(unsafe {
                                        ipc::wire::assume_init_slice(&__buf, 2 + ipc::RawHandle::SIZE)
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
            };

            quote! {
                #enum_name::#variant => {
                    #deserialize
                    #destructure
                    #(#lease_bindings)*
                    #(#peer_resolution)*
                    #acquire_block
                    #ctor_body
                }
            }
        }

        MethodKind::Message => {
            let (deserialize, destructure) = gen_deserialize_args(&non_lease_params, true);
            let effective_rt = server_return_type(m);
            let handle_result = gen_reply(method_name, &call_args, effective_rt.as_ref(), false);

            quote! {
                #enum_name::#variant => {
                    #deserialize
                    #destructure
                    #(#lease_bindings)*
                    #(#peer_resolution)*
                    #acquire_block
                    #handle_result
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
                    #(#lease_bindings)*
                    #(#peer_resolution)*
                    #acquire_block
                    #handle_result
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
                    #(#peer_resolution)*
                    #acquire_block
                    #static_reply
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

/// Generate sequential zerocopy reads for message arguments.
///
/// Emits a series of `ipc::wire::read::<T>(__buf)` calls, threading the
/// remaining buffer through each read.  No tuples, no serde, no hubpack.
fn gen_deserialize_args(
    non_lease_params: &[&ParsedParam],
    include_handle: bool,
) -> (TokenStream2, TokenStream2) {
    let mut reads = Vec::new();

    // Start from raw message bytes
    reads.push(quote! { let mut __buf = msg.raw_data(); });

    if include_handle {
        reads.push(quote! {
            let Some((handle, __rest)) = ipc::wire::read::<ipc::RawHandle>(__buf) else {
                reply.reply_error(ipc::MALFORMED_MESSAGE, &[]);
                return;
            };
            __buf = __rest;
        });
    }

    for p in non_lease_params {
        let pname = &p.name;
        let ty = wire_type(p);
        reads.push(quote! {
            let Some((#pname, __rest)) = ipc::wire::read::<#ty>(__buf) else {
                reply.reply_error(ipc::MALFORMED_MESSAGE, &[]);
                return;
            };
            __buf = __rest;
        });
    }

    let deserialize = quote! { #(#reads)* };
    // No destructure step needed — variables are already bound individually
    (deserialize, quote! {})
}

fn gen_lease_bindings(lease_params: &[&ParsedParam]) -> Vec<TokenStream2> {
    lease_params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let pname = &p.name;
            if p.lease_mutable {
                quote! {
                    let #pname = match msg.lease::<ipc::dispatch::Write>(#i) {
                        Ok(v) => v,
                        Err(_) => { reply.reply_error(ipc::MALFORMED_MESSAGE, &[]); return; }
                    };
                }
            } else {
                quote! {
                    let #pname = match msg.lease::<ipc::dispatch::Read>(#i) {
                        Ok(v) => v,
                        Err(_) => { reply.reply_error(ipc::MALFORMED_MESSAGE, &[]); return; }
                    };
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
        let encode = crate::util::gen_encode_return_value(rt, quote! { result_value });
        quote! {
            let result_value = T::#method_name(meta, #(#call_args),*);
            let mut __reply_buf: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
                unsafe { core::mem::MaybeUninit::uninit().assume_init() };
            let mut __off = 0usize;
            ipc::wire::set_uninit(&mut __reply_buf, __off, 0); // Result::Ok tag
            __off += 1;
            #encode
            reply.reply_ok(unsafe { ipc::wire::assume_init_slice(&__reply_buf, __off) });
        }
    } else {
        quote! {
            T::#method_name(meta, #(#call_args),*);
            reply.reply_ok(&[0u8]); // Result::Ok(())
        }
    }
}

/// Helper: generate code to reply with `Err(ipc::Error::HandleLost)`.
fn gen_reply_handle_lost() -> TokenStream2 {
    quote! {
        reply.reply_ok(&[1u8, ipc::Error::HandleLost as u8]); // Result::Err(HandleLost)
        return;
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

    let handle_lost = gen_reply_handle_lost();

    if let Some(rt) = return_type {
        let encode = crate::util::gen_encode_return_value(rt, quote! { result_value });
        quote! {
            let Some(resource) = #arena_op else {
                #handle_lost
            };
            let result_value = resource.#method_name(meta, #(#call_args),*);
            let mut __reply_buf: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
                unsafe { core::mem::MaybeUninit::uninit().assume_init() };
            let mut __off = 0usize;
            ipc::wire::set_uninit(&mut __reply_buf, __off, 0); // Result::Ok tag
            __off += 1;
            #encode
            reply.reply_ok(unsafe { ipc::wire::assume_init_slice(&__reply_buf, __off) });
        }
    } else {
        quote! {
            let Some(resource) = #arena_op else {
                #handle_lost
            };
            resource.#method_name(meta, #(#call_args),*);
            reply.reply_ok(&[0u8]); // Result::Ok(())
        }
    }
}
