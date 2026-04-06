#![allow(clippy::too_many_arguments)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse_macro_input;

mod client;
mod emit_meta;
mod lease;
mod parse;
mod resolve_acl;
mod resolve_alloc;
mod resolve_priority;
mod server;
mod server_macro;
mod transfer;
mod util;
mod wire_format;

use client::{gen_client, gen_dyn_client};
use parse::{InterfaceAttr, MethodKind, ResourceAttr, parse_methods};
use server::{gen_constants, gen_dispatcher, gen_operation_enum, gen_server_trait, gen_wiring_macro};
use util::to_screaming_snake_case;

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

    // Emit metadata for post-build handle ACL verification.
    {
        let crate_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
        let implements_str = attrs
            .implements
            .as_ref()
            .and_then(|p| p.segments.last().map(|s| s.ident.to_string()));
        let clone_str = attrs.clone_mode.map(|_| "refcount");
        let handle_params: Vec<emit_meta::HandleParam> = methods
            .iter()
            .flat_map(|m| {
                m.params.iter().filter_map(|p| {
                    let mode = p.handle_mode?;
                    Some(emit_meta::HandleParam {
                        method: m.name.to_string(),
                        handle_trait: p
                            .impl_trait_name
                            .as_ref()
                            .map(|i| i.to_string())
                            .unwrap_or_else(|| "(concrete)".to_string()),
                        mode: match mode {
                            parse::HandleMode::Move => "move".to_string(),
                            parse::HandleMode::Clone => "clone".to_string(),
                        },
                    })
                })
            })
            .collect();
        emit_meta::emit_resource(
            &crate_name,
            &trait_name.to_string(),
            attrs.kind,
            implements_str.as_deref(),
            clone_str,
            &handle_params,
        );
    }

    let server_trait = gen_server_trait(trait_name, &methods, &attrs);
    let op_enum = gen_operation_enum(trait_name, &methods, &attrs);
    let dispatcher = gen_dispatcher(trait_name, &methods, &attrs);
    let client = gen_client(trait_name, &methods, &attrs);
    let constants = gen_constants(trait_name, &attrs);
    let wiring_macro = gen_wiring_macro(trait_name, &methods);

    let output = quote! {
        #server_trait
        #op_enum
        #dispatcher
        #client
        #constants
        #wiring_macro
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

    // Emit metadata for post-build handle ACL verification.
    {
        let crate_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
        emit_meta::emit_interface(&crate_name, &trait_name.to_string(), iface_attrs.kind);
    }

    let attrs = ResourceAttr {
        arena_size: None,
        kind: iface_attrs.kind,
        implements: None,
        clone_mode: None,
    };

    let server_trait = gen_server_trait(trait_name, &methods, &attrs);
    let op_enum = gen_operation_enum(trait_name, &methods, &attrs);
    let client = gen_dyn_client(trait_name, &methods, &attrs);

    let output = quote! {
        #server_trait
        #op_enum
        #client
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

    // Emit metadata for post-build handle ACL verification.
    {
        let task_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
        let serves: Vec<String> = input
            .entries
            .iter()
            .map(|e| e.trait_name.to_string())
            .collect();
        emit_meta::emit_server(&task_name, &serves);
    }

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
            if __notif.group_id != generated::#group_id_const {
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

/// Internal proc macro invoked by generated `bind_X!` macros.
///
/// Checks that the consuming crate has declared a dependency on the named
/// task via `uses-sysmodule` in `app.kdl`. Reads `.work/app.uses.json`
/// at compile time. If the file doesn't exist (e.g. during IDE cargo check),
/// enforcement is silently skipped.
#[proc_macro]
pub fn __check_uses(input: TokenStream) -> TokenStream {
    let lit = parse_macro_input!(input as syn::LitStr);
    let dep_task = lit.value();

    let result = (|| -> Result<Option<String>, (bool, String)> {
        let manifest_dir =
            std::env::var("CARGO_MANIFEST_DIR").map_err(|e| (true, e.to_string()))?;
        let project_root = std::path::PathBuf::from(&manifest_dir)
            .ancestors()
            .find(|p| p.join(".work").exists())
            .ok_or_else(|| (true, "no .work directory found".to_string()))?
            .to_path_buf();

        let json_path = project_root.join(".work").join("app.uses.json");
        let content = std::fs::read_to_string(&json_path).map_err(|e| {
            let is_not_found = e.kind() == std::io::ErrorKind::NotFound;
            (is_not_found, e.to_string())
        })?;

        let pkg_name = std::env::var("CARGO_PKG_NAME").map_err(|e| (true, e.to_string()))?;

        let root: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| (false, format!("failed to parse app.uses.json: {}", e)))?;
        let obj = root
            .as_object()
            .ok_or_else(|| (false, "app.uses.json is not a JSON object".to_string()))?;

        let deps = match obj.get(&pkg_name) {
            Some(v) => v.as_array().ok_or_else(|| {
                (
                    false,
                    format!("app.uses.json: value for '{}' is not an array", pkg_name),
                )
            })?,
            None => return Ok(None),
        };

        if deps.iter().any(|d| d.as_str() == Some(&dep_task)) {
            Ok(None)
        } else {
            let short_name = dep_task.strip_prefix("sysmodule_").unwrap_or(&dep_task);
            Ok(Some(format!(
                "This task does not declare a dependency on `{}`. \
                 Add `uses-sysmodule \"{}\"` to your task in app.kdl.",
                dep_task, short_name,
            )))
        }
    })();

    match result {
        Ok(Some(err_msg)) => {
            let err = syn::Error::new(lit.span(), err_msg);
            err.to_compile_error().into()
        }
        Ok(None) => TokenStream::new(),
        Err((true, _)) => TokenStream::new(),
        Err((false, msg)) => {
            syn::Error::new(proc_macro::Span::call_site().into(), msg)
                .to_compile_error()
                .into()
        }
    }
}

// ===========================================================================
// ipc::allocation!(...)
// ===========================================================================

/// Declare a static handle to a named memory allocation.
///
/// Syntax: `ipc::allocation!(NAME = @alloc_name: Type);`
#[proc_macro]
pub fn allocation(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as AllocationInput);

    let static_name = &parsed.static_name;
    let alloc_name_str = parsed.alloc_name.to_string();
    let ty = &parsed.ty;

    // Check that this task has uses-allocation in app.kdl
    match resolve_alloc::check_acl(&alloc_name_str) {
        Ok(Some(err_msg)) => {
            return syn::Error::new(parsed.alloc_name.span(), err_msg)
                .to_compile_error()
                .into();
        }
        Ok(None) => {} // allowed
        Err(_) => {}   // file missing, skip check
    }

    let info = match resolve_alloc::resolve(&alloc_name_str) {
        Ok(Some(info)) => info,
        Ok(None) => {
            let msg = format!("unknown allocation '{}'", alloc_name_str);
            return syn::Error::new(parsed.alloc_name.span(), msg)
                .to_compile_error()
                .into();
        }
        Err(_) => {
            return quote! {
                static #static_name: () = ();
            }
            .into();
        }
    };

    let alloc_base = info.base as usize;
    let alloc_size = info.size as usize;
    let alloc_align = info.align as usize;

    let size_msg = format!(
        "size mismatch: type size != allocation '{}' size ({} bytes)",
        alloc_name_str, alloc_size,
    );
    let align_msg = format!(
        "alignment mismatch: type alignment > allocation '{}' alignment ({} bytes)",
        alloc_name_str, alloc_align,
    );

    let wrapper_type = format_ident!("__Alloc_{}", static_name);

    let sentinel_ident =
        format_ident!("__only_one_usage_allowed_for_allocation_{}", alloc_name_str);

    let output = quote! {
        const _: () = assert!(
            core::mem::size_of::<#ty>() == #alloc_size,
            #size_msg,
        );
        const _: () = assert!(
            core::mem::align_of::<#ty>() <= #alloc_align,
            #align_msg,
        );

        #[allow(non_camel_case_types)]
        struct #wrapper_type;

        impl #wrapper_type {
            /// Take the allocation. Returns `None` if already taken.
            fn get(&self) -> Option<&'static mut core::mem::MaybeUninit<#ty>> {
                static TAKEN: core::sync::atomic::AtomicBool =
                    core::sync::atomic::AtomicBool::new(false);
                ipc::alloc_take::take::<#ty>(&TAKEN, #alloc_base)
            }
        }

        static #static_name: #wrapper_type = #wrapper_type;

        #[unsafe(no_mangle)]
        #[unsafe(link_section = ".discard")]
        static #sentinel_ident: () = ();
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
