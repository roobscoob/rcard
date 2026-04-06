use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::{ParsedMethod, collect_peer_traits};

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
