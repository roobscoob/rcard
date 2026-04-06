use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::{MethodKind, ParsedMethod, ResourceAttr, interface_op_path};
use crate::util::to_pascal_case;

pub fn gen_operation_enum(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    attrs: &ResourceAttr,
) -> TokenStream2 {
    let enum_name = format_ident!("{}Op", trait_name);
    let method_count = methods.len() as u8;
    let method_count_lit = proc_macro2::Literal::u8_suffixed(method_count);

    let iface_op = attrs.implements.as_ref().map(interface_op_path);

    let mut non_message_offset: u8 = 0;

    let variants: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            if let Some(ref iface_op) = iface_op {
                if m.kind == MethodKind::Message || m.kind == MethodKind::StaticMessage {
                    quote! { #variant = #iface_op::#variant as u8 }
                } else {
                    let offset = proc_macro2::Literal::u8_suffixed(non_message_offset);
                    non_message_offset += 1;
                    quote! { #variant = #iface_op::METHOD_COUNT + #offset }
                }
            } else {
                let id = m.method_id;
                quote! { #variant = #id }
            }
        })
        .collect();

    let const_bindings: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            let const_name = format_ident!("__{}", m.name.to_string().to_uppercase());
            quote! { const #const_name: u8 = #enum_name::#variant as u8; }
        })
        .collect();

    let match_arms: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
            let const_name = format_ident!("__{}", m.name.to_string().to_uppercase());
            quote! { #const_name => Ok(#enum_name::#variant) }
        })
        .collect();

    quote! {
        #[derive(Copy, Clone, Debug)]
        #[repr(u8)]
        pub enum #enum_name {
            #(#variants),*
        }

        impl #enum_name {
            pub const METHOD_COUNT: u8 = #method_count_lit;
        }

        impl TryFrom<u8> for #enum_name {
            type Error = u8;
            fn try_from(x: u8) -> core::result::Result<Self, Self::Error> {
                #(#const_bindings)*
                match x {
                    #(#match_arms,)*
                    other => Err(other),
                }
            }
        }
    }
}
