use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::Ident;

use crate::parse::{MethodKind, ParsedMethod, ResourceAttr, collect_peer_traits, parse_slice_ref};
use crate::util::replace_ident_in_type;

use super::peers::{is_resolvable_peer, peer_generic_name};

pub fn gen_server_trait(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    _attrs: &ResourceAttr,
) -> TokenStream2 {
    let peers = collect_peer_traits(methods);

    let peer_generics: Vec<TokenStream2> = peers
        .iter()
        .map(|(name, _)| {
            let g = peer_generic_name(name);
            quote! { #g }
        })
        .collect();

    let method_fns: Vec<TokenStream2> = methods.iter().map(gen_trait_method).collect();

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

fn gen_trait_method(m: &ParsedMethod) -> TokenStream2 {
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
            let peer_g = peer_generic_name(p.impl_trait_name.as_ref().unwrap());
            params.push(quote! { #pname: &#peer_g });
        } else if p.handle_mode.is_some() {
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
            let replaced = replace_ident_in_type(rt, generic_ident, &quote! { ipc::RawHandle });
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
}
