#![allow(clippy::too_many_arguments)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse_macro_input;

mod client;
mod lease;
mod metadata_json;
mod parse;
mod schema_export;
mod section;
mod server;
mod server_macro;
mod transfer;
mod util;
mod wire_format;

use client::{gen_client, gen_dyn_client};
use parse::{InterfaceAttr, MethodKind, ResourceAttr, parse_methods};
use server::{gen_constants, gen_dispatcher, gen_operation_enum, gen_server_trait, gen_wiring_macro};
use util::{cfg_gate_firmware_only, to_screaming_snake_case};

// ===========================================================================
// #[ipc::resource(...)]
// ===========================================================================

#[proc_macro_attribute]
pub fn resource(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = parse_macro_input!(attr as ResourceAttr);
    let trait_def = parse_macro_input!(item as syn::ItemTrait);
    let trait_name = &trait_def.ident;

    let methods = match parse_methods(&trait_def) {
        Ok(m) => m,
        Err(e) => return e.to_compile_error().into(),
    };

    if attrs.arena_size.is_none() {
        return syn::Error::new_spanned(
            &trait_def.ident,
            "ipc::resource requires `arena_size`. Use `#[ipc::interface]` for interface-only traits.",
        )
        .to_compile_error()
        .into();
    }

    // Validate clone = refcount constraints.
    if attrs.clone_mode == Some(parse::CloneMode::Refcount) {
        for m in &methods {
            if m.kind == MethodKind::Destructor {
                return syn::Error::new(
                    m.name.span(),
                    "refcounted resources cannot have explicit destructors; \
                     they are freed when the last reference is released",
                )
                .to_compile_error()
                .into();
            }
        }
    }

    let server_trait = gen_server_trait(trait_name, &methods, &attrs);
    let op_enum = gen_operation_enum(trait_name, &methods, &attrs);
    let dispatcher = gen_dispatcher(trait_name, &methods, &attrs);
    let client = gen_client(trait_name, &methods, &attrs);
    let constants = gen_constants(trait_name, &attrs);
    let wiring_macro = gen_wiring_macro(trait_name, &methods);

    let meta_json = metadata_json::resource_record(trait_name, &methods, &attrs, false);
    let meta_section = section::emit(&trait_name.to_string(), &meta_json);
    let schema_export = schema_export::gen_schema_export(trait_name, &methods, &attrs);

    // The server trait, op enum, and constants are pure type-level
    // content that compiles on any target. The dispatcher, client, and
    // wiring macro reference kernel-only types (sys_send, StaticTaskId,
    // Meta with TaskId, ACCESS_VIOLATION, etc.) and are firmware-only.
    //
    // Each firmware-only chunk is emitted with an individual
    // `#[cfg(target_os = "none")]` attribute so host builds elide them
    // cleanly while keeping the items at module scope when present.
    // The op enum and constants are pure type-level content that
    // compiles on any target — the host needs them for method-ID lookup.
    //
    // The server trait, dispatcher, client, and wiring macro reference
    // kernel-only types (Meta, dispatch::LeaseBorrow, sys_send,
    // StaticTaskId, ACCESS_VIOLATION, etc.) and are firmware-only.
    // Each individual item gets its own `#[cfg(target_os = "none")]`
    // via cfg_gate_firmware_only.
    let firmware_only = cfg_gate_firmware_only(quote! {
        #server_trait
        #dispatcher
        #client
        #wiring_macro
    });

    let output = quote! {
        #op_enum
        #constants
        #meta_section
        #schema_export
        #firmware_only
    };

    output.into()
}

// ===========================================================================
// #[ipc::interface(...)]
// ===========================================================================

#[proc_macro_attribute]
pub fn interface(attr: TokenStream, item: TokenStream) -> TokenStream {
    let iface_attrs = parse_macro_input!(attr as InterfaceAttr);
    let trait_def = parse_macro_input!(item as syn::ItemTrait);
    let trait_name = &trait_def.ident;

    let methods = match parse_methods(&trait_def) {
        Ok(m) => m,
        Err(e) => return e.to_compile_error().into(),
    };

    let attrs = ResourceAttr {
        arena_size: None,
        kind: iface_attrs.kind,
        implements: None,
        clone_mode: None,
    };

    let server_trait = gen_server_trait(trait_name, &methods, &attrs);
    let op_enum = gen_operation_enum(trait_name, &methods, &attrs);
    let client = gen_dyn_client(trait_name, &methods, &attrs);

    let meta_json = metadata_json::resource_record(trait_name, &methods, &attrs, true);
    let meta_section = section::emit(&trait_name.to_string(), &meta_json);
    let schema_export = schema_export::gen_schema_export(trait_name, &methods, &attrs);

    // server_trait references `Meta` (firmware-only) and client uses
    // dispatch/call_send (firmware-only). Gate both per-item so host
    // builds (schema-dump, host/ipc-runtime) elide them cleanly.
    let firmware_only = cfg_gate_firmware_only(quote! {
        #server_trait
        #client
    });

    let output = quote! {
        #op_enum
        #meta_section
        #schema_export
        #firmware_only
    };

    output.into()
}

// ===========================================================================
// ipc::server!(...)
// ===========================================================================

/// Construct and run an IPC server from a list of `TraitName: ConcreteType` pairs.
///
/// Each trait must have been declared with `#[ipc::resource(...)]`, which
/// generates the dispatcher, constants, and wiring macro needed here.
///
/// ```ignore
/// ipc::server! {
///     FileSystemRegistry: RegistryResource,
///     FileSystem: FileSystemResource,
///     File: FileResource,
///     Folder: FolderResource,
///     FolderIterator: FolderIteratorResource,
/// }
/// .run()
/// ```
#[proc_macro]
pub fn server(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as server_macro::ServerInput);

    server_macro::gen_server(&input).into()
}

// ===========================================================================
// #[ipc::notification_handler(group_name)]
// ===========================================================================

/// Transforms a notification handler function.
///
/// The input function must have the signature:
/// ```ignore
/// fn handler_name(sender: u16, code: u32) { ... }
/// ```
///
/// The attribute transforms it into a function that takes `&Notification`
/// and only executes the body when `notif.group_id` matches the specified
/// group. The group ID is resolved from `generated::GROUP_ID_<SCREAMING_NAME>`.
#[proc_macro_attribute]
pub fn notification_handler(attr: TokenStream, item: TokenStream) -> TokenStream {
    let group_name = parse_macro_input!(attr as syn::Ident);
    let func = parse_macro_input!(item as syn::ItemFn);

    let fn_name = &func.sig.ident;
    let fn_vis = &func.vis;
    let fn_body = &func.block;

    let screaming = to_screaming_snake_case(&group_name.to_string());
    let group_id_const = format_ident!("GROUP_ID_{}", screaming);

    let output = quote! {
        #fn_vis fn #fn_name(__notif: &sysmodule_reactor_api::Notification) {
            if __notif.group_id != ::generated::notifications::#group_id_const {
                return;
            }
            let sender = __notif.sender_index;
            let code = __notif.code;
            #fn_body
        }
    };

    output.into()
}

// ===========================================================================
// ipc::__check_uses!(...)
// ===========================================================================

/// No-op — dependency validation is handled by the Nickel config structure
/// and ACL enforcement is at runtime via `generated::acl`.
#[proc_macro]
pub fn __check_uses(_input: TokenStream) -> TokenStream {
    TokenStream::new()
}

// ===========================================================================
// ipc::allocation!(...)
// ===========================================================================

/// Declare a static in a named memory region.
///
/// The linker places the data in the region assigned by `tfw::layout`.
/// At runtime, `get()` returns a one-shot `&'static mut` reference.
///
/// Syntax: `ipc::allocation!(NAME = @region_name: Type);`
#[proc_macro]
pub fn allocation(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as AllocationInput);

    let static_name = &parsed.static_name;
    let alloc_name_str = parsed.alloc_name.to_string();
    let ty = &parsed.ty;

    let section_name = format!(".{alloc_name_str}");
    let wrapper_type = format_ident!("__Alloc_{}", static_name);
    let storage_ident = format_ident!("__alloc_storage_{}", alloc_name_str);

    let output = quote! {
        #[allow(non_camel_case_types)]
        struct #wrapper_type;

        impl #wrapper_type {
            /// Take the allocation. Returns `None` if already taken.
            fn get(&self) -> Option<&'static mut core::mem::MaybeUninit<#ty>> {
                static TAKEN: core::sync::atomic::AtomicBool =
                    core::sync::atomic::AtomicBool::new(false);
                use core::ops::Not;
                TAKEN
                    .swap(true, core::sync::atomic::Ordering::Relaxed)
                    .not()
                    .then(|| unsafe { &mut *core::ptr::addr_of_mut!(#storage_ident) })
            }
        }

        #[unsafe(link_section = #section_name)]
        static mut #storage_ident: core::mem::MaybeUninit<#ty> =
            core::mem::MaybeUninit::uninit();

        static #static_name: #wrapper_type = #wrapper_type;
    };

    output.into()
}

struct AllocationInput {
    static_name: syn::Ident,
    alloc_name: syn::Ident,
    ty: syn::Type,
}

impl syn::parse::Parse for AllocationInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let static_name: syn::Ident = input.parse()?;
        input.parse::<syn::Token![=]>()?;
        input.parse::<syn::Token![@]>()?;
        let alloc_name: syn::Ident = input.parse()?;
        input.parse::<syn::Token![:]>()?;
        let ty: syn::Type = input.parse()?;
        Ok(AllocationInput {
            static_name,
            alloc_name,
            ty,
        })
    }
}
