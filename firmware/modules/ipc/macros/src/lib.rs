use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse_macro_input;

mod codegen_client;
mod codegen_server;
mod emit_meta;
mod parse;
mod resolve_acl;
mod resolve_alloc;
mod resolve_priority;
mod util;

use codegen_client::{gen_client, gen_dyn_client};
use codegen_server::{
    gen_constants, gen_dispatcher, gen_operation_enum, gen_server_trait, gen_wiring_macro,
};
use parse::{parse_methods, InterfaceAttr, MethodKind, ResourceAttr};
use util::to_screaming_snake_case;

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
        let implements_str = attrs.implements.as_ref().and_then(|p| {
            p.segments.last().map(|s| s.ident.to_string())
        });
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

    // Build a ResourceAttr for codegen compatibility (no arena, no dispatcher).
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

// ---------------------------------------------------------------------------
// server! proc macro
// ---------------------------------------------------------------------------

struct ServerEntry {
    trait_name: syn::Ident,
    concrete_type: syn::Path,
}

struct NotificationConfig {
    /// The reactor client type (e.g., `Reactor` from `bind_reactor!`).
    reactor_client: syn::Path,
    /// Handler functions to call for each pulled notification.
    handlers: Vec<syn::Path>,
}

struct ServerInput {
    entries: Vec<ServerEntry>,
    notifications: Option<NotificationConfig>,
}

impl syn::parse::Parse for ServerInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut entries = Vec::new();
        let mut notifications = None;

        while !input.is_empty() {
            // Check for @notifications
            if input.peek(syn::Token![@]) {
                input.parse::<syn::Token![@]>()?;
                let kw: syn::Ident = input.parse()?;
                if kw != "notifications" {
                    return Err(syn::Error::new(kw.span(), "expected `notifications`"));
                }

                // Parse (ReactorClient)
                let content;
                syn::parenthesized!(content in input);
                let reactor_client: syn::Path = content.parse()?;

                input.parse::<syn::Token![=>]>()?;

                // Parse handler1, handler2, ...
                let mut handlers = Vec::new();
                handlers.push(input.parse::<syn::Path>()?);
                while input.peek(syn::Token![,]) && !input.is_empty() {
                    input.parse::<syn::Token![,]>()?;
                    if input.is_empty() || input.peek(syn::Token![@]) {
                        break;
                    }
                    // Check if next token is an ident followed by `:` (a ServerEntry)
                    // If so, break out — this comma was the trailing comma before the next entry
                    if input.peek(syn::Ident) && input.peek2(syn::Token![:]) {
                        break;
                    }
                    handlers.push(input.parse::<syn::Path>()?);
                }

                notifications = Some(NotificationConfig {
                    reactor_client,
                    handlers,
                });
            } else {
                let trait_name: syn::Ident = input.parse()?;
                input.parse::<syn::Token![:]>()?;
                let concrete_type: syn::Path = input.parse()?;
                entries.push(ServerEntry {
                    trait_name,
                    concrete_type,
                });
                if !input.is_empty() {
                    input.parse::<syn::Token![,]>()?;
                }
            }
        }
        Ok(ServerInput {
            entries,
            notifications,
        })
    }
}

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
    let input = parse_macro_input!(input as ServerInput);

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

    let count = input.entries.len();

    let mut arena_decls = Vec::new();
    let mut dispatcher_decls = Vec::new();
    let mut register_calls = Vec::new();

    // Collect all trait names for the wiring macro calls
    let all_trait_names: Vec<&syn::Ident> = input.entries.iter().map(|e| &e.trait_name).collect();

    for entry in &input.entries {
        let trait_name = &entry.trait_name;
        let concrete_type = &entry.concrete_type;

        let snake = util::to_snake_case(&trait_name.to_string());
        let screaming = util::to_screaming_snake_case(&trait_name.to_string());

        let arena_var = format_ident!("__arena_{}", snake);
        let disp_var = format_ident!("__disp_{}", snake);
        let kind_const = format_ident!("{}_KIND", screaming);
        let arena_size_const = format_ident!("{}_ARENA_SIZE", screaming);
        let wiring_macro = format_ident!("__new_{}Dispatcher", trait_name);

        arena_decls.push(quote! {
            let #arena_var = ipc::SharedArena::<
                #concrete_type, { #arena_size_const }
            >::new(#kind_const);
        });

        // Build all-arenas key-value list for the wiring macro
        let arena_kvs: Vec<proc_macro2::TokenStream> = all_trait_names
            .iter()
            .map(|tn| {
                let tn_snake = util::to_snake_case(&tn.to_string());
                let tn_arena = format_ident!("__arena_{}", tn_snake);
                quote! { #tn => &#tn_arena }
            })
            .collect();

        dispatcher_decls.push(quote! {
            let mut #disp_var = #wiring_macro!(
                &#arena_var, __ipc_priority_for, __ipc_self_task_index;
                #(#arena_kvs),*
            );
        });

        register_calls.push(quote! {
            __server.register(#kind_const, &mut #disp_var);
        });
    }

    let run_call = if let Some(notif_cfg) = &input.notifications {
        let reactor = &notif_cfg.reactor_client;
        let handlers = &notif_cfg.handlers;
        quote! {
            __server.run_with_notifications(
                &mut __buf,
                sysmodule_reactor_api::NOTIFICATION_BIT,
                |_bits| {
                    loop {
                        match #reactor::pull() {
                            Ok(Some(notif)) => {
                                #( #handlers(&notif); )*
                            }
                            _ => break,
                        }
                    }
                },
            )
        }
    } else {
        quote! { __server.run(&mut __buf) }
    };

    // Generate __ipc_priority_for function from app.priorities.json
    let priority_fn = gen_priority_fn();
    // Generate __ipc_acl_check function from app.uses.json + app.peers.json
    let acl_fn = gen_acl_fn();
    // Generate __ipc_self_task_index constant from HUBRIS_TASKS + CARGO_PKG_NAME
    let self_task_index_const = gen_self_task_index();

    let output = quote! {
        {
            #priority_fn
            #acl_fn
            #self_task_index_const

            #(#arena_decls)*
            #(#dispatcher_decls)*

            let mut __buf = [core::mem::MaybeUninit::uninit(); 256];
            let mut __server = ipc::Server::<#count>::new(__ipc_acl_check);
            #(#register_calls)*
            #run_call
        }
    };

    output.into()
}

/// Generate a `__ipc_priority_for(sender_index: u16) -> i8` function
/// by reading `.work/app.priorities.json` and `HUBRIS_TASKS` at compile time.
fn gen_priority_fn() -> proc_macro2::TokenStream {
    match resolve_priority::resolve() {
        Ok(entries) if !entries.is_empty() => {
            let arms: Vec<proc_macro2::TokenStream> = entries
                .iter()
                .map(|e| {
                    let idx = e.task_index as u16;
                    let prio = e.priority as i8;
                    quote! { #idx => #prio }
                })
                .collect();
            quote! {
                fn __ipc_priority_for(sender_index: u16) -> i8 {
                    match sender_index {
                        #(#arms,)*
                        _ => 0,
                    }
                }
            }
        }
        Ok(_) => {
            // No entries — default everything to 0
            quote! {
                fn __ipc_priority_for(_sender_index: u16) -> i8 { 0 }
            }
        }
        Err(msg) => {
            // File missing (IDE cargo check, no prior build) → silently skip.
            // Malformed JSON → compile error so it doesn't go unnoticed.
            if msg.contains("cannot read") {
                quote! {
                    fn __ipc_priority_for(_sender_index: u16) -> i8 { 0 }
                }
            } else {
                let err = format!("ipc: failed to resolve priorities: {}", msg);
                quote! { compile_error!(#err); }
            }
        }
    }
}

/// Generate a `__ipc_acl_check(sender_index: u16) -> bool` function
/// by reading `.work/app.uses.json`, `.work/app.peers.json`, and `HUBRIS_TASKS`
/// at compile time.
fn gen_acl_fn() -> proc_macro2::TokenStream {
    match resolve_acl::resolve() {
        Ok(allowed) if !allowed.is_empty() => {
            let arms: Vec<proc_macro2::TokenStream> = allowed
                .iter()
                .map(|&idx| {
                    let idx = idx as u16;
                    quote! { #idx => true }
                })
                .collect();
            quote! {
                fn __ipc_acl_check(sender_index: u16) -> bool {
                    match sender_index {
                        #(#arms,)*
                        _ => false,
                    }
                }
            }
        }
        Ok(_) => {
            // No entries — no clients declared. Deny all by default.
            quote! {
                fn __ipc_acl_check(_sender_index: u16) -> bool { false }
            }
        }
        Err(msg) => {
            // File missing (IDE cargo check, no prior build) → silently skip.
            // Malformed JSON → compile error so it doesn't go unnoticed.
            if msg.contains("cannot read") {
                quote! {
                    fn __ipc_acl_check(_sender_index: u16) -> bool { false }
                }
            } else {
                let err = format!("ipc: failed to resolve ACL: {}", msg);
                quote! { compile_error!(#err); }
            }
        }
    }
}

/// Generate a `__ipc_self_task_index: u16` constant by looking up
/// `CARGO_PKG_NAME` in the `HUBRIS_TASKS` environment variable.
fn gen_self_task_index() -> proc_macro2::TokenStream {
    let pkg = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let task_names: Vec<String> = std::env::var("HUBRIS_TASKS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.to_string())
        .collect();

    if let Some(idx) = task_names.iter().position(|t| t == &pkg) {
        let idx = idx as u16;
        quote! {
            let __ipc_self_task_index: u16 = #idx;
        }
    } else {
        // Fallback: task not found (IDE cargo check, no HUBRIS_TASKS) → use 0
        quote! {
            let __ipc_self_task_index: u16 = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// #[notification_handler(group_name)] proc macro attribute
// ---------------------------------------------------------------------------

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
///
/// Example:
/// ```ignore
/// #[notification_handler(logs)]
/// fn handle_log(sender: u16, code: u32) {
///     // only called for the "logs" notification group
/// }
/// ```
///
/// Convention: the subscriber task must have a `generated` module
/// (from build.rs) containing `GROUP_ID_LOGS: u16` constants.
#[proc_macro_attribute]
pub fn notification_handler(attr: TokenStream, item: TokenStream) -> TokenStream {
    let group_name = parse_macro_input!(attr as syn::Ident);
    let func = parse_macro_input!(item as syn::ItemFn);

    let fn_name = &func.sig.ident;
    let fn_vis = &func.vis;
    let fn_body = &func.block;

    // Convert group name to SCREAMING_SNAKE for the generated constant
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

// ---------------------------------------------------------------------------
// __check_uses! – dependency enforcement at consumer compile time
// ---------------------------------------------------------------------------

/// Internal proc macro invoked by generated `bind_X!` macros.
///
/// Checks that the consuming crate has declared a dependency on the named
/// task via `uses-sysmodule` in `app.kdl`. Reads `.work/app.uses.json`
/// at compile time. If the file doesn't exist (e.g. during IDE cargo check),
/// enforcement is silently skipped.
///
/// Usage (generated, not user-facing):
/// ```ignore
/// ipc_macros::__check_uses!("sysmodule_fs");
/// ```
#[proc_macro]
pub fn __check_uses(input: TokenStream) -> TokenStream {
    let lit = parse_macro_input!(input as syn::LitStr);
    let dep_task = lit.value();

    // Inline the check logic so we can distinguish file-not-found (skip)
    // from JSON parse errors (compile error).
    let result = (|| -> Result<Option<String>, (bool, String)> {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
            .map_err(|e| (true, e.to_string()))?;
        let project_root = std::path::PathBuf::from(&manifest_dir)
            .ancestors()
            .find(|p| p.join(".work").exists())
            .ok_or_else(|| (true, "no .work directory found".to_string()))?
            .to_path_buf();

        let json_path = project_root.join(".work").join("app.uses.json");
        let content = std::fs::read_to_string(&json_path).map_err(|e| {
            // File not found is expected during IDE cargo check
            let is_not_found = e.kind() == std::io::ErrorKind::NotFound;
            (is_not_found, e.to_string())
        })?;

        let pkg_name = std::env::var("CARGO_PKG_NAME")
            .map_err(|e| (true, e.to_string()))?;

        // JSON parse errors should be compile errors, not silently skipped
        let root: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| (false, format!("failed to parse app.uses.json: {}", e)))?;
        let obj = root.as_object()
            .ok_or_else(|| (false, "app.uses.json is not a JSON object".to_string()))?;

        let deps = match obj.get(&pkg_name) {
            Some(v) => v.as_array()
                .ok_or_else(|| (false, format!("app.uses.json: value for '{}' is not an array", pkg_name)))?,
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
        Err((true, _)) => {
            // File not found or env var missing → skip (expected during IDE cargo check)
            TokenStream::new()
        }
        Err((false, msg)) => {
            // JSON parse error → compile error
            syn::Error::new(proc_macro::Span::call_site().into(), msg)
                .to_compile_error()
                .into()
        }
    }
}

// ---------------------------------------------------------------------------
// ipc::allocation!() – declare a handle to a named memory allocation
// ---------------------------------------------------------------------------

/// Declare a static handle to a named memory allocation.
///
/// Syntax: `ipc::allocation!(NAME = @alloc_name: Type);`
///
/// Example:
/// ```ignore
/// ipc::allocation!(FRAME_BUFFERS = @frame_buffers: [[u8; 8192]; 64]);
/// let fb = FRAME_BUFFERS.get(); // &'static mut [[u8; 8192]; 64]
/// ```
///
/// The allocation must be defined in `app.kdl` and the task must have
/// `uses-allocation "alloc_name"`. Compile-time checks verify that
/// `size_of::<Type>() == allocation size` and `align_of::<Type>() <= allocation align`.
/// Calling `.get()` twice panics.
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

    // Look up the allocation for compile-time checks and base address
    let info = match resolve_alloc::resolve(&alloc_name_str) {
        Ok(Some(info)) => info,
        Ok(None) => {
            let msg = format!("unknown allocation '{}'", alloc_name_str);
            return syn::Error::new(parsed.alloc_name.span(), msg)
                .to_compile_error()
                .into();
        }
        // JSON not available (e.g. IDE cargo check) — emit a dummy static
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

    // Sentinel symbol: prevents two statics from using the same allocation
    // within one binary. The linker will error on duplicate symbols.
    let sentinel_ident = format_ident!("__only_one_usage_allowed_for_allocation_{}", alloc_name_str);

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
            /// Take the allocation. Panics if called twice.
            fn get(&self) -> &'static mut core::mem::MaybeUninit<#ty> {
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

/// Parse: `NAME = @alloc_name: Type`
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
