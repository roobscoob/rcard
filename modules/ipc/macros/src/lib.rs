use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;

mod codegen_client;
mod codegen_server;
mod parse;
mod util;

use codegen_client::gen_client;
use codegen_server::{gen_dispatcher, gen_operation_enum, gen_server_trait};
use parse::{parse_methods, ResourceAttr};

#[proc_macro_attribute]
pub fn resource(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs = parse_macro_input!(attr as ResourceAttr);
    let trait_def = parse_macro_input!(item as syn::ItemTrait);
    let trait_name = &trait_def.ident;

    let methods = parse_methods(&trait_def);

    let server_trait = gen_server_trait(trait_name, &methods);
    let op_enum = gen_operation_enum(trait_name, &methods);
    let dispatcher = gen_dispatcher(trait_name, &methods, attrs.arena_size);
    let client = gen_client(trait_name, &methods, attrs.kind);

    let output = quote! {
        #server_trait
        #op_enum
        #dispatcher
        #client
    };

    output.into()
}
