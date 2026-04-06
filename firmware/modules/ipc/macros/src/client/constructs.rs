use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::Ident;

/// Generate the mapping expression that converts a wire value `v`
/// (containing `RawHandle`) into the user-facing type (containing typed handles).
///
/// For `Option<FS>`: maps `v` from `Option<RawHandle>` to
/// `Option<FileSystemHandle<FS>>` via `v.map(|h| Handle::from_raw(h))`.
pub fn gen_constructs_map(
    return_type: Option<&syn::Type>,
    generic_ident: &Ident,
    handle_type: &Ident,
) -> TokenStream2 {
    let Some(rt) = return_type else {
        return quote! { v };
    };

    if let syn::Type::Path(p) = rt {
        if let Some(seg) = p.path.segments.last() {
            if seg.ident == "Option" {
                return quote! {
                    v.map(|raw| #handle_type::<#generic_ident>::from_raw(raw))
                };
            }
            if seg.ident == "Result" {
                return quote! {
                    v.map(|raw| #handle_type::<#generic_ident>::from_raw(raw))
                };
            }
        }
        // Bare FS → Handle::from_raw(v)
        if p.path
            .get_ident()
            .map(|i| i == generic_ident)
            .unwrap_or(false)
        {
            return quote! {
                #handle_type::<#generic_ident>::from_raw(v)
            };
        }
    }

    // Fallback: return as-is
    quote! { v }
}
