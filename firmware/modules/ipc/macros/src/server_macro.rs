use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::{metadata_json, section, util};

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

pub struct ServerEntry {
    pub trait_name: Ident,
    pub concrete_type: syn::Path,
}

pub struct NotificationConfig {
    pub reactor_client: syn::Path,
    pub handlers: Vec<syn::Path>,
}

pub struct IrqConfig {
    /// Qualified IRQ name (e.g. `usbc_irq`) — resolved at codegen time via
    /// `generated::irq_bit!(crate_name, irq_name)` to a notification bit.
    pub name: Ident,
    /// Closure invoked when the IRQ notification fires. The macro re-arms
    /// the IRQ via `sys_enable_irq` after each call.
    pub handler: syn::Expr,
}

pub struct ServerInput {
    pub entries: Vec<ServerEntry>,
    pub notifications: Option<NotificationConfig>,
    pub irq: Option<IrqConfig>,
}

impl syn::parse::Parse for ServerInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut entries = Vec::new();
        let mut notifications = None;
        let mut irq = None;

        while !input.is_empty() {
            if input.peek(syn::Token![@]) {
                input.parse::<syn::Token![@]>()?;
                let kw: Ident = input.parse()?;
                if kw == "notifications" {
                    let content;
                    syn::parenthesized!(content in input);
                    let reactor_client: syn::Path = content.parse()?;

                    input.parse::<syn::Token![=>]>()?;

                    let mut handlers = Vec::new();
                    handlers.push(input.parse::<syn::Path>()?);
                    while input.peek(syn::Token![,]) && !input.is_empty() {
                        input.parse::<syn::Token![,]>()?;
                        if input.is_empty() || input.peek(syn::Token![@]) {
                            break;
                        }
                        if input.peek(syn::Ident) && input.peek2(syn::Token![:]) {
                            break;
                        }
                        handlers.push(input.parse::<syn::Path>()?);
                    }

                    notifications = Some(NotificationConfig {
                        reactor_client,
                        handlers,
                    });
                } else if kw == "irq" {
                    let content;
                    syn::parenthesized!(content in input);
                    let irq_name: Ident = content.parse()?;
                    input.parse::<syn::Token![=>]>()?;
                    let handler: syn::Expr = input.parse()?;
                    irq = Some(IrqConfig {
                        name: irq_name,
                        handler,
                    });
                    if input.peek(syn::Token![,]) {
                        input.parse::<syn::Token![,]>()?;
                    }
                } else {
                    return Err(syn::Error::new(
                        kw.span(),
                        "expected `notifications` or `irq`",
                    ));
                }
            } else {
                let trait_name: Ident = input.parse()?;
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
            irq,
        })
    }
}

// ---------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------

pub fn gen_server(input: &ServerInput) -> TokenStream2 {
    let count = input.entries.len();

    let mut arena_decls = Vec::new();
    let mut dispatcher_decls = Vec::new();
    let mut register_calls = Vec::new();

    let all_trait_names: Vec<&Ident> = input.entries.iter().map(|e| &e.trait_name).collect();

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

        // Emit the arena as a function-scope `static`, not a local. This
        // parks the arena in BSS (zero-initialized at link time via the
        // const constructor) instead of constructing it on main's stack
        // at runtime. For a Log-sized arena that's ~800 B removed from
        // main's permanent frame, plus the ~2.3 KB one-shot peak from
        // `SharedArena::new` building a temporary on its own stack.
        arena_decls.push(quote! {
            static #arena_var: ipc::SharedArena<
                #concrete_type, { #arena_size_const }
            > = ipc::SharedArena::new(#kind_const);
        });

        let arena_kvs: Vec<TokenStream2> = all_trait_names
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

    let pkg_ident = format_ident!(
        "{}",
        std::env::var("CARGO_PKG_NAME")
            .unwrap_or_default()
            .replace('-', "_")
    );

    let run_call = match (input.notifications.as_ref(), input.irq.as_ref()) {
        (None, None) => quote! { __server.run(&mut __buf) },
        (Some(notif_cfg), None) => {
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
        }
        (None, Some(irq_cfg)) => {
            let irq_name = &irq_cfg.name;
            let handler = &irq_cfg.handler;
            quote! {
                {
                    const __IRQ_BIT: u32 = ::generated::irq_bit!(#pkg_ident, #irq_name);
                    ::userlib::sys_enable_irq_and_clear_pending(__IRQ_BIT);
                    let mut __irq_handler = #handler;
                    __server.run_with_notifications(
                        &mut __buf,
                        __IRQ_BIT,
                        |__bits| {
                            if __bits & __IRQ_BIT != 0 {
                                __irq_handler();
                                ::userlib::sys_enable_irq(__IRQ_BIT);
                            }
                        },
                    )
                }
            }
        }
        (Some(notif_cfg), Some(irq_cfg)) => {
            let reactor = &notif_cfg.reactor_client;
            let handlers = &notif_cfg.handlers;
            let irq_name = &irq_cfg.name;
            let handler = &irq_cfg.handler;
            quote! {
                {
                    const __IRQ_BIT: u32 = ::generated::irq_bit!(#pkg_ident, #irq_name);
                    ::userlib::sys_enable_irq_and_clear_pending(__IRQ_BIT);
                    let mut __irq_handler = #handler;
                    __server.run_with_notifications(
                        &mut __buf,
                        __IRQ_BIT | sysmodule_reactor_api::NOTIFICATION_BIT,
                        |__bits| {
                            if __bits & __IRQ_BIT != 0 {
                                __irq_handler();
                                ::userlib::sys_enable_irq(__IRQ_BIT);
                            }
                            if __bits & sysmodule_reactor_api::NOTIFICATION_BIT != 0 {
                                loop {
                                    match #reactor::pull() {
                                        Ok(Some(notif)) => {
                                            #( #handlers(&notif); )*
                                        }
                                        _ => break,
                                    }
                                }
                            }
                        },
                    )
                }
            }
        }
    };

    let priority_fn = gen_priority_fn();
    let acl_fn = gen_acl_fn();
    let self_task_index_const = gen_self_task_index();

    // Metadata record: which task serves which resource traits. Emitted
    // into `.ipc_meta` so the tfw builder can collate task → resource
    // mappings into `ipc-metadata.json`.
    let task_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let serves: Vec<String> = input
        .entries
        .iter()
        .map(|e| e.trait_name.to_string())
        .collect();
    let meta_json = metadata_json::server_record(&task_name, &serves);
    let tag = format!("server_{}", task_name.replace('-', "_"));
    let meta_section = section::emit(&tag, &meta_json);

    quote! {
        {
            #priority_fn
            #acl_fn
            #self_task_index_const

            #meta_section

            #(#arena_decls)*
            #(#dispatcher_decls)*

            let mut __buf = [core::mem::MaybeUninit::uninit(); 256];
            let mut __server = ipc::Server::<#count>::new(__ipc_acl_check);
            #(#register_calls)*
            #run_call
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime resolution via `generated` crate
// ---------------------------------------------------------------------------

fn gen_priority_fn() -> TokenStream2 {
    // Priority is no longer resolved at compile time.
    // All callers get default priority 0.
    // TODO: if per-caller priorities are needed, add them to generated::tasks.
    quote! {
        fn __ipc_priority_for(_sender_index: u16) -> i8 { 0 }
    }
}

fn gen_acl_fn() -> TokenStream2 {
    let pkg_ident = format_ident!("{}", std::env::var("CARGO_PKG_NAME")
        .unwrap_or_default()
        .replace('-', "_"));

    quote! {
        fn __ipc_acl_check(sender_index: u16) -> bool {
            ::generated::acl_check!(#pkg_ident, sender_index)
        }
    }
}

fn gen_self_task_index() -> TokenStream2 {
    // Look up our own task index from the generated task list at runtime.
    let pkg = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let pkg_lit = proc_macro2::Literal::string(&pkg);
    quote! {
        let __ipc_self_task_index: u16 = ::generated::tasks::TASK_NAMES
            .iter()
            .position(|&n| n == #pkg_lit)
            .unwrap_or(0) as u16;
    }
}
