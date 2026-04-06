use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::parse::ParsedParam;

/// Determine the wire type for a parameter.
///
/// Handle parameters become `RawHandle` (concrete) or `DynHandle` (impl Trait).
/// All other parameters pass through unchanged.
pub fn wire_type_for(p: &ParsedParam) -> TokenStream2 {
    if p.handle_mode.is_some() {
        if p.impl_trait_name.is_some() {
            quote! { ipc::DynHandle }
        } else {
            quote! { ipc::RawHandle }
        }
    } else {
        let ty = &p.ty;
        quote! { #ty }
    }
}

/// If `ty` is `Option<T>`, return the inner `T`.
pub fn extract_option_inner(ty: &syn::Type) -> Option<&syn::Type> {
    if let syn::Type::Path(tp) = ty {
        let seg = tp.path.segments.last()?;
        if seg.ident == "Option" {
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                    return Some(inner);
                }
            }
        }
    }
    None
}

/// If `ty` is `Result<T, E>`, return `(T, E)`.
pub fn extract_result_types(ty: &syn::Type) -> Option<(&syn::Type, &syn::Type)> {
    if let syn::Type::Path(tp) = ty {
        let seg = tp.path.segments.last()?;
        if seg.ident == "Result" {
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                let mut iter = args.args.iter();
                if let (
                    Some(syn::GenericArgument::Type(ok_ty)),
                    Some(syn::GenericArgument::Type(err_ty)),
                ) = (iter.next(), iter.next())
                {
                    return Some((ok_ty, err_ty));
                }
            }
        }
    }
    None
}
