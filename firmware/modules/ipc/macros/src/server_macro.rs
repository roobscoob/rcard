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

/// Periodic-tick handler. The macro arms a kernel timer (`sys_set_timer`)
/// for `period_ms` in the future on entry and re-arms it after every fire,
/// so the dispatcher wakes at fixed intervals between IPC traffic. The
/// schedule is monotonic (next deadline = previous deadline + period), so
/// short overruns don't accumulate drift; long stalls advance to `now + 1`.
pub struct IntervalConfig {
    /// Tick period in milliseconds.
    pub period_ms: u64,
    /// Function called on every tick. Takes no arguments.
    pub handler: syn::Path,
}

pub struct ServerInput {
    pub entries: Vec<ServerEntry>,
    pub notifications: Option<NotificationConfig>,
    pub irq: Option<IrqConfig>,
    pub interval: Option<IntervalConfig>,
}

impl syn::parse::Parse for ServerInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut entries = Vec::new();
        let mut notifications = None;
        let mut irq = None;
        let mut interval = None;

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
                } else if kw == "interval" {
                    let content;
                    syn::parenthesized!(content in input);
                    let lit: syn::LitInt = content.parse()?;
                    let value: u64 = lit.base10_parse()?;
                    let period_ms = match lit.suffix() {
                        "" | "ms" => value,
                        "s" => value.saturating_mul(1000),
                        "us" => value / 1000,
                        other => {
                            return Err(syn::Error::new(
                                lit.span(),
                                format!(
                                    "interval: unknown duration suffix `{other}` (use ms/s/us)"
                                ),
                            ));
                        }
                    };
                    if period_ms == 0 {
                        return Err(syn::Error::new(
                            lit.span(),
                            "interval: period rounds to 0 ms",
                        ));
                    }
                    input.parse::<syn::Token![=>]>()?;
                    let handler: syn::Path = input.parse()?;
                    interval = Some(IntervalConfig { period_ms, handler });
                    if input.peek(syn::Token![,]) {
                        input.parse::<syn::Token![,]>()?;
                    }
                } else {
                    return Err(syn::Error::new(
                        kw.span(),
                        "expected `notifications`, `irq`, or `interval`",
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
            interval,
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

    // Compose the dispatcher loop from the optional notification / irq /
    // interval pieces. Each contributes:
    //   - prelude: declarations / setup that runs before `run_with_notifications`
    //   - mask term: a bit that joins the `notification_mask` argument
    //   - closure branch: a `if __bits & FOO != 0 { … }` block in the closure
    let mut prelude_pieces: Vec<TokenStream2> = Vec::new();
    let mut mask_terms: Vec<TokenStream2> = Vec::new();
    let mut closure_branches: Vec<TokenStream2> = Vec::new();

    if let Some(notif_cfg) = input.notifications.as_ref() {
        let reactor = &notif_cfg.reactor_client;
        let handlers = &notif_cfg.handlers;
        mask_terms.push(quote! { sysmodule_reactor_api::NOTIFICATION_BIT });
        closure_branches.push(quote! {
            if __bits & sysmodule_reactor_api::NOTIFICATION_BIT != 0 {
                loop {
                    match #reactor::pull() {
                        Ok(Some(notif)) => { #( #handlers(&notif); )* }
                        _ => break,
                    }
                }
            }
        });
    }

    if let Some(irq_cfg) = input.irq.as_ref() {
        let pkg_ident = format_ident!(
            "{}",
            std::env::var("CARGO_PKG_NAME")
                .unwrap_or_default()
                .replace('-', "_")
        );
        let irq_name = &irq_cfg.name;
        let handler = &irq_cfg.handler;
        prelude_pieces.push(quote! {
            const __IRQ_BIT: u32 = ::generated::irq_bit!(#pkg_ident, #irq_name);
            ::userlib::sys_enable_irq_and_clear_pending(__IRQ_BIT);
            let mut __irq_handler = #handler;
        });
        mask_terms.push(quote! { __IRQ_BIT });
        closure_branches.push(quote! {
            if __bits & __IRQ_BIT != 0 {
                __irq_handler();
                ::userlib::sys_enable_irq(__IRQ_BIT);
            }
        });
    }

    if let Some(int_cfg) = input.interval.as_ref() {
        let period_ms = int_cfg.period_ms;
        let handler = &int_cfg.handler;
        // Bit 30 is reserved for the interval timer; reactor uses bit 31
        // (`NOTIFICATION_BIT`) and IRQs use lower bits assigned by the build.
        prelude_pieces.push(quote! {
            const __INTERVAL_BIT: u32 = 1u32 << 30;
            const __INTERVAL_PERIOD_MS: u64 = #period_ms;
            let mut __next_interval_deadline: u64 =
                ::userlib::sys_get_timer().now + __INTERVAL_PERIOD_MS;
            ::userlib::sys_set_timer(
                ::core::option::Option::Some(__next_interval_deadline),
                __INTERVAL_BIT,
            );
        });
        mask_terms.push(quote! { __INTERVAL_BIT });
        closure_branches.push(quote! {
            if __bits & __INTERVAL_BIT != 0 {
                #handler();
                let __now = ::userlib::sys_get_timer().now;
                // Monotonic schedule: small overruns don't drift; long
                // stalls jump to (now + period) so we don't burn CPU
                // catching up on missed ticks.
                __next_interval_deadline = ::core::cmp::max(
                    __next_interval_deadline + __INTERVAL_PERIOD_MS,
                    __now + __INTERVAL_PERIOD_MS,
                );
                ::userlib::sys_set_timer(
                    ::core::option::Option::Some(__next_interval_deadline),
                    __INTERVAL_BIT,
                );
            }
        });
    }

    let run_call = if mask_terms.is_empty() {
        quote! { __server.run(&mut __buf) }
    } else {
        quote! {
            {
                #( #prelude_pieces )*
                __server.run_with_notifications(
                    &mut __buf,
                    0u32 #( | #mask_terms )*,
                    |__bits| {
                        #( #closure_branches )*
                    },
                )
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
