use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::{metadata_json, section, util};

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

pub struct AsyncServerEntry {
    pub trait_name: Ident,
    pub concrete_type: syn::Path,
}

pub struct AsyncNotificationConfig {
    pub reactor_client: syn::Path,
    pub handlers: Vec<syn::Path>,
}

pub struct AsyncIrqConfig {
    pub name: Ident,
    pub handler: syn::Expr,
}

pub struct AsyncTaskConfig {
    pub exprs: Vec<syn::Expr>,
}

pub struct AsyncServerInput {
    pub entries: Vec<AsyncServerEntry>,
    pub notifications: Option<AsyncNotificationConfig>,
    pub irq: Option<AsyncIrqConfig>,
    pub async_tasks: Option<AsyncTaskConfig>,
}

impl syn::parse::Parse for AsyncServerInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut entries = Vec::new();
        let mut notifications = None;
        let mut irq = None;
        let mut async_tasks = None;

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

                    notifications = Some(AsyncNotificationConfig {
                        reactor_client,
                        handlers,
                    });
                } else if kw == "irq" {
                    let content;
                    syn::parenthesized!(content in input);
                    let irq_name: Ident = content.parse()?;
                    input.parse::<syn::Token![=>]>()?;
                    let handler: syn::Expr = input.parse()?;
                    irq = Some(AsyncIrqConfig {
                        name: irq_name,
                        handler,
                    });
                    if input.peek(syn::Token![,]) {
                        input.parse::<syn::Token![,]>()?;
                    }
                } else if kw == "spawn" {
                    input.parse::<syn::Token![=>]>()?;
                    let content;
                    syn::bracketed!(content in input);
                    let mut exprs = Vec::new();
                    exprs.push(content.parse::<syn::Expr>()?);
                    while content.peek(syn::Token![,]) {
                        content.parse::<syn::Token![,]>()?;
                        if content.is_empty() {
                            break;
                        }
                        exprs.push(content.parse::<syn::Expr>()?);
                    }
                    async_tasks = Some(AsyncTaskConfig { exprs });
                    if input.peek(syn::Token![,]) {
                        input.parse::<syn::Token![,]>()?;
                    }
                } else {
                    return Err(syn::Error::new(
                        kw.span(),
                        "expected `notifications`, `irq`, or `spawn`",
                    ));
                }
            } else {
                let trait_name: Ident = input.parse()?;
                input.parse::<syn::Token![:]>()?;
                let concrete_type: syn::Path = input.parse()?;
                entries.push(AsyncServerEntry {
                    trait_name,
                    concrete_type,
                });
                if !input.is_empty() {
                    input.parse::<syn::Token![,]>()?;
                }
            }
        }
        Ok(AsyncServerInput {
            entries,
            notifications,
            irq,
            async_tasks,
        })
    }
}

// ---------------------------------------------------------------------------
// Code generation
// ---------------------------------------------------------------------------

pub fn gen_async_server(input: &AsyncServerInput) -> TokenStream2 {
    let count = input.entries.len();

    // --- Arena + dispatcher setup (same as server!) ---
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

    // --- IRQ setup ---
    let mut prelude = Vec::new();
    let mut mask_terms = Vec::new();
    let mut irq_branch = quote! {};

    if let Some(irq_cfg) = input.irq.as_ref() {
        let pkg_ident = format_ident!(
            "{}",
            std::env::var("CARGO_PKG_NAME")
                .unwrap_or_default()
                .replace('-', "_")
        );
        let irq_name = &irq_cfg.name;
        let handler = &irq_cfg.handler;
        prelude.push(quote! {
            const __IRQ_BIT: u32 = ::generated::irq_bit!(#pkg_ident, #irq_name);
            ::userlib::sys_enable_irq_and_clear_pending(__IRQ_BIT);
            let mut __irq_handler = #handler;
        });
        mask_terms.push(quote! { __IRQ_BIT });
        irq_branch = quote! {
            if __bits & __IRQ_BIT != 0 {
                __irq_handler();
                ::userlib::sys_enable_irq(__IRQ_BIT);
            }
        };
    }

    // --- Reactor notifications ---
    let mut reactor_branch = quote! {};
    if let Some(notif_cfg) = input.notifications.as_ref() {
        let reactor = &notif_cfg.reactor_client;
        let handlers = &notif_cfg.handlers;
        mask_terms.push(quote! { sysmodule_reactor_api::NOTIFICATION_BIT });
        reactor_branch = quote! {
            if __bits & sysmodule_reactor_api::NOTIFICATION_BIT != 0 {
                loop {
                    match #reactor::pull() {
                        Ok(Some(notif)) => { #( #handlers(&notif); )* }
                        _ => break,
                    }
                }
            }
        };
    }

    // --- Async futures + executor ---
    let task_count = input
        .async_tasks
        .as_ref()
        .map(|t| t.exprs.len())
        .unwrap_or(0);

    let mut future_decls = Vec::new();
    let mut poll_branches = Vec::new();

    if let Some(async_cfg) = input.async_tasks.as_ref() {
        for (i, expr) in async_cfg.exprs.iter().enumerate() {
            let future_var = format_ident!("__future_{}", i);
            future_decls.push(quote! {
                let mut #future_var = #expr;
            });
            poll_branches.push(quote! {
                if __ready & (1u32 << #i) != 0 {
                    let __w = __executor.make_waker(#i);
                    let __cx = &mut ::core::task::Context::from_waker(&__w);
                    let _ = unsafe {
                        ::core::pin::Pin::new_unchecked(&mut #future_var)
                    }.poll(__cx);
                }
            });
        }
    }

    // --- Compose the run loop ---
    let priority_fn = gen_priority_fn();
    let acl_fn = gen_acl_fn();
    let self_task_index_const = gen_self_task_index();

    let task_name = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let serves: Vec<String> = input
        .entries
        .iter()
        .map(|e| e.trait_name.to_string())
        .collect();
    let meta_json = metadata_json::server_record(&task_name, &serves);
    let tag = format!("server_{}", task_name.replace('-', "_"));
    let meta_section = section::emit(&tag, &meta_json);

    let task_count_lit = task_count;

    quote! {
        {
            #priority_fn
            #acl_fn
            #self_task_index_const

            #meta_section

            #(#arena_decls)*
            #(#dispatcher_decls)*

            let mut __buf = [::core::mem::MaybeUninit::uninit(); 256];
            let mut __server = ipc::Server::<#count>::new(__ipc_acl_check);
            #(#register_calls)*

            #(#prelude)*

            // Async futures on the stack (never moved — main is -> !)
            #(#future_decls)*

            // Executor state
            let mut __executor =
                ipc::executor::ExecutorState::<#task_count_lit>::new(__ipc_self_task_index as u16);
            __executor.fixup_waker_pointers();
            __executor.mark_all_ready();

            let __full_mask: u32 = 0u32
                | ipc::executor::EXECUTOR_BIT
                | ipc::executor::TIMER_BIT
                #( | #mask_terms )*;

            loop {
                // Poll all ready futures
                loop {
                    let __ready = __executor.ready_mask.swap(
                        0, ::core::sync::atomic::Ordering::AcqRel,
                    );
                    if __ready == 0 { break; }
                    #(#poll_branches)*
                }
                __executor.timer_queue.arm_kernel_timer();

                // Block until message or notification
                match ipc::kern::sys_recv_open(&mut __buf, __full_mask) {
                    ipc::kern::MessageOrNotification::Notification(__bits) => {
                        if __bits & ipc::executor::TIMER_BIT != 0 {
                            // Expire executor-managed timers
                            let __now = ::userlib::sys_get_timer().now;
                            let __expired = __executor.timer_queue.expire(__now);
                            __executor.ready_mask.fetch_or(
                                __expired,
                                ::core::sync::atomic::Ordering::Release,
                            );
                            // Expire embassy-time driver timers
                            ipc::executor::time_driver::on_timer_tick();
                        }
                        // EXECUTOR_BIT: ready_mask already set by waker
                        #irq_branch
                        #reactor_branch
                    }
                    ipc::kern::MessageOrNotification::Message(ref __msg) => {
                        __server.dispatch_message(__msg);
                    }
                }

                // IPC handlers may have signaled channels
                if __executor.ready_mask.load(::core::sync::atomic::Ordering::Acquire) != 0 {
                    continue;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers (duplicated from server_macro — keep in sync)
// ---------------------------------------------------------------------------

fn gen_priority_fn() -> TokenStream2 {
    quote! {
        fn __ipc_priority_for(_sender_index: u16) -> i8 { 0 }
    }
}

fn gen_acl_fn() -> TokenStream2 {
    let pkg_ident = format_ident!(
        "{}",
        std::env::var("CARGO_PKG_NAME")
            .unwrap_or_default()
            .replace('-', "_")
    );

    quote! {
        fn __ipc_acl_check(sender_index: u16) -> bool {
            ::generated::acl_check!(#pkg_ident, sender_index)
        }
    }
}

fn gen_self_task_index() -> TokenStream2 {
    let pkg = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    let pkg_lit = proc_macro2::Literal::string(&pkg);
    quote! {
        let __ipc_self_task_index: u16 = ::generated::tasks::TASK_NAMES
            .iter()
            .position(|&n| n == #pkg_lit)
            .unwrap_or(0) as u16;
    }
}
