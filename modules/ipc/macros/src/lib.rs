use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse_macro_input;

mod codegen_client;
mod codegen_server;
mod parse;
mod util;

use codegen_client::{gen_client, gen_dyn_client};
use codegen_server::{
    gen_constants, gen_dispatcher, gen_operation_enum, gen_server_trait, gen_wiring_macro,
};
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

// ---------------------------------------------------------------------------
// server! proc macro
// ---------------------------------------------------------------------------

struct ServerEntry {
    trait_name: syn::Ident,
    concrete_type: syn::Path,
}

struct ServerInput {
    entries: Vec<ServerEntry>,
}

impl syn::parse::Parse for ServerInput {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut entries = Vec::new();
        while !input.is_empty() {
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
        Ok(ServerInput { entries })
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
                &#arena_var;
                #(#arena_kvs),*
            );
        });

        register_calls.push(quote! {
            __server.register(#kind_const, &mut #disp_var);
        });
    }

    let output = quote! {
        {
            #(#arena_decls)*
            #(#dispatcher_decls)*

            let mut __buf = [core::mem::MaybeUninit::uninit(); 256];
            let mut __server = ipc::Server::<#count>::new();
            #(#register_calls)*
            __server.run(&mut __buf)
        }
    };

    output.into()
}
