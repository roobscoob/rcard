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

// Note: `extract_option_inner` and `extract_result_types` were removed
// when the wire format switched from zerocopy to postcard. Postcard
// handles `Option<T>` and `Result<T, E>` natively via serde's variant
// encoding, so the macro no longer needs to special-case them.
