use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::ResourceAttr;
use crate::util::to_screaming_snake_case;

pub fn gen_constants(trait_name: &Ident, attrs: &ResourceAttr) -> TokenStream2 {
    let arena_size = attrs.arena_size.unwrap_or(0);
    let kind = attrs.kind;
    let screaming = to_screaming_snake_case(&trait_name.to_string());
    let kind_name = format_ident!("{}_KIND", screaming);
    let arena_size_name = format_ident!("{}_ARENA_SIZE", screaming);
    let kind_lit = proc_macro2::Literal::u8_suffixed(kind);

    quote! {
        pub const #kind_name: u8 = #kind_lit;
        pub const #arena_size_name: usize = #arena_size;
    }
}
