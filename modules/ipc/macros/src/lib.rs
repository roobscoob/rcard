use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;

mod codegen_client;
mod codegen_server;
mod parse;
mod util;

use codegen_client::{gen_client, gen_dyn_client};
use codegen_server::{gen_dispatcher, gen_operation_enum, gen_server_trait};
use parse::{parse_methods, InterfaceAttr, MethodKind, ResourceAttr};

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

    let output = quote! {
        #server_trait
        #op_enum
        #dispatcher
        #client
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

    // Build a ResourceAttr for codegen compatibility (no arena, no dispatcher).
    let attrs = ResourceAttr {
        arena_size: None,
        kind: iface_attrs.kind,
        implements: None,
        clone_mode: None,
        parent: None,
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
