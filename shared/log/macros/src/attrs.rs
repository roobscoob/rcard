use std::collections::BTreeMap;
use syn::{Attribute, Expr, ExprLit, Lit, MetaNameValue};

/// Extract `#[format(key = "value", ...)]` hint attributes into a JSON-friendly map.
pub fn parse_format_hints(attrs: &[Attribute]) -> BTreeMap<String, String> {
    let mut hints = BTreeMap::new();

    for attr in attrs {
        if !attr.path().is_ident("format") {
            continue;
        }

        let nested = match attr.parse_args_with(
            syn::punctuated::Punctuated::<MetaNameValue, syn::Token![,]>::parse_terminated,
        ) {
            Ok(n) => n,
            Err(_) => continue,
        };

        for nv in nested {
            let key = nv
                .path
                .get_ident()
                .map(|i| i.to_string())
                .unwrap_or_default();

            let value = match &nv.value {
                Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) => s.value(),
                _ => continue,
            };

            hints.insert(key, value);
        }
    }

    hints
}
