use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::Ident;

/// Returns the token path for the panic macro to use in generated code.
/// Always emits `ipc::__ipc_panic` which dispatches based on the
/// `bare-panics` feature on the **consumer** crate (not ipc or the
/// proc-macro crate), giving per-task control.
pub fn panic_path() -> TokenStream2 {
    quote! { ipc::__ipc_panic }
}

/// Replace occurrences of `target_ident` in a type with `replacement`.
/// Handles `Option<FS>` → `Option<ipc::RawHandle>`, etc.
pub fn replace_ident_in_type(
    ty: &syn::Type,
    target: &Ident,
    replacement: &TokenStream2,
) -> TokenStream2 {
    match ty {
        syn::Type::Path(p) => {
            if let Some(ident) = p.path.get_ident() {
                if ident == target {
                    return replacement.clone();
                }
            }
            // Recurse into generic arguments.
            let segments: Vec<TokenStream2> = p
                .path
                .segments
                .iter()
                .map(|seg| {
                    let ident = &seg.ident;
                    match &seg.arguments {
                        syn::PathArguments::AngleBracketed(args) => {
                            let inner: Vec<TokenStream2> = args
                                .args
                                .iter()
                                .map(|arg| match arg {
                                    syn::GenericArgument::Type(inner_ty) => {
                                        replace_ident_in_type(inner_ty, target, replacement)
                                    }
                                    other => quote! { #other },
                                })
                                .collect();
                            quote! { #ident<#(#inner),*> }
                        }
                        _ => quote! { #seg },
                    }
                })
                .collect();
            quote! { #(#segments)::* }
        }
        _ => quote! { #ty },
    }
}

pub fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

pub fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(ch.to_lowercase().next().unwrap());
        } else {
            result.push(ch);
        }
    }
    result
}

pub fn to_screaming_snake_case(s: &str) -> String {
    to_snake_case(s).to_uppercase()
}

pub fn to_snake_ident(ident: &Ident) -> Ident {
    Ident::new(&to_snake_case(&ident.to_string()), ident.span())
}
