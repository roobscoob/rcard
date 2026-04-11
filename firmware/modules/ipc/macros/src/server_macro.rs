use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::util;

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

pub struct ServerInput {
    pub entries: Vec<ServerEntry>,
    pub notifications: Option<NotificationConfig>,
}

impl syn::parse::Parse for ServerInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut entries = Vec::new();
        let mut notifications = None;

        while !input.is_empty() {
            if input.peek(syn::Token![@]) {
                input.parse::<syn::Token![@]>()?;
                let kw: Ident = input.parse()?;
                if kw != "notifications" {
                    return Err(syn::Error::new(kw.span(), "expected `notifications`"));
                }

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

        arena_decls.push(quote! {
            let #arena_var = ipc::SharedArena::<
                #concrete_type, { #arena_size_const }
            >::new(#kind_const);
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

    let priority_fn = gen_priority_fn();
    let acl_fn = gen_acl_fn();
    let self_task_index_const = gen_self_task_index();

    quote! {
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
