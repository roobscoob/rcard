use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields};

use crate::attrs::parse_format_hints;

pub fn derive(input: DeriveInput) -> TokenStream {
    let name = &input.ident;
    let name_str = name.to_string();

    match &input.data {
        Data::Struct(data) => derive_struct(name, &name_str, &input.attrs, &data.fields),
        Data::Enum(data) => derive_enum(name, &name_str, &input.attrs, data),
        Data::Union(_) => {
            syn::Error::new_spanned(&input, "Format cannot be derived for unions")
                .to_compile_error()
        }
    }
}

/// Deterministic hash for a metadata key (e.g. "type:MyStruct", "field:MyStruct.x").
fn meta_hash(key: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// Emit a sidecar JSON file for a metadata entry and return its hash as a u64 literal.
fn emit_meta(filename_prefix: &str, key: &str, json: serde_json::Value) -> u64 {
    let hash = meta_hash(key);
    let hash_hex = format!("0x{:016x}", hash);
    crate::sidecar::emit(
        &format!("{}.{:016x}.json", filename_prefix, hash),
        &serde_json::json!({
            "id": hash_hex,
            "entry": json,
        }),
    );
    hash
}

fn derive_struct(
    name: &syn::Ident,
    name_str: &str,
    attrs: &[syn::Attribute],
    fields: &Fields,
) -> TokenStream {
    let struct_hints = parse_format_hints(attrs);

    let named_fields: Vec<_> = match fields {
        Fields::Named(f) => f.named.iter().collect(),
        Fields::Unnamed(f) => f.unnamed.iter().collect(),
        Fields::Unit => vec![],
    };

    let is_named = matches!(fields, Fields::Named(_));
    let field_count = named_fields.len();

    // Build field metadata JSON entries
    let mut field_json_entries = Vec::new();
    for (i, field) in named_fields.iter().enumerate() {
        let field_name = if is_named {
            field.ident.as_ref().unwrap().to_string()
        } else {
            i.to_string()
        };
        let hints = parse_format_hints(&field.attrs);
        let entry = serde_json::json!({
            "name": field_name,
            "index": i,
            "hints": hints,
        });
        field_json_entries.push(entry);
    }

    // Type-level metadata → sidecar
    let type_json = serde_json::json!({
        "kind": "struct",
        "name": name_str,
        "hints": struct_hints,
        "fields": field_json_entries,
    });
    let type_hash: u64 = emit_meta("type", &format!("type:{}", name_str), type_json);

    // Per-field metadata + format body
    let mut format_body = Vec::new();

    for (i, field) in named_fields.iter().enumerate() {
        let field_name_str = if is_named {
            field.ident.as_ref().unwrap().to_string()
        } else {
            i.to_string()
        };

        let field_json = serde_json::json!({
            "kind": "field",
            "type": name_str,
            "name": field_name_str,
            "index": i,
        });
        let field_hash: u64 = emit_meta(
            "field",
            &format!("field:{}.{}", name_str, field_name_str),
            field_json,
        );

        let tmp = format_ident!("__field{}", i);
        let field_copy = if is_named {
            let ident = field.ident.as_ref().unwrap();
            quote! { let #tmp = self.#ident; }
        } else {
            let index = syn::Index::from(i);
            quote! { let #tmp = self.#index; }
        };

        format_body.push(quote! {
            #field_copy
            __f.write_field_id(#field_hash);
            rcard_log::formatter::Format::format(&#tmp, __f);
        });
    }

    let field_count_lit = field_count as u64;

    // Choose with_struct for named/unit, with_tuple for unnamed
    let format_call = if is_named || matches!(fields, Fields::Unit) {
        quote! {
            __f.with_struct(
                #type_hash,
                #field_count_lit,
                |__f| {
                    #(#format_body)*
                },
            );
        }
    } else {
        // Tuple struct: no field_ids, use with_tuple
        let tuple_format_body: Vec<_> = named_fields
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let index = syn::Index::from(i);
                let tmp = format_ident!("__field{}", i);
                quote! {
                    let #tmp = self.#index;
                    rcard_log::formatter::Format::format(&#tmp, __f);
                }
            })
            .collect();
        quote! {
            __f.with_tuple(
                #type_hash,
                #field_count_lit,
                |__f| {
                    #(#tuple_format_body)*
                },
            );
        }
    };

    quote! {
        impl rcard_log::formatter::Format for #name {
            fn format<W: rcard_log::formatter::Writer>(&self, __f: &mut rcard_log::formatter::Formatter<W>) {
                #format_call
            }
        }
    }
}

fn derive_enum(
    name: &syn::Ident,
    name_str: &str,
    attrs: &[syn::Attribute],
    data: &syn::DataEnum,
) -> TokenStream {
    let enum_hints = parse_format_hints(attrs);

    let variant_names: Vec<String> = data.variants.iter().map(|v| v.ident.to_string()).collect();

    // Enum-level metadata → sidecar
    let enum_json = serde_json::json!({
        "kind": "enum",
        "name": name_str,
        "hints": enum_hints,
        "variants": variant_names,
    });
    let _enum_hash: u64 = emit_meta("type", &format!("type:{}", name_str), enum_json);

    let mut match_arms = Vec::new();

    for variant in &data.variants {
        let variant_ident = &variant.ident;
        let variant_name_str = variant_ident.to_string();

        let variant_key = format!("variant:{}::{}", name_str, variant_name_str);

        match &variant.fields {
            Fields::Unit => {
                let variant_json = serde_json::json!({
                    "kind": "variant",
                    "enum": name_str,
                    "name": variant_name_str,
                    "style": "unit",
                });
                let variant_hash: u64 = emit_meta("variant", &variant_key, variant_json);

                match_arms.push(quote! {
                    #name::#variant_ident => {
                        __f.with_struct(
                            #variant_hash,
                            0,
                            |_| {},
                        );
                    }
                });
            }

            Fields::Named(fields) => {
                let field_count = fields.named.len() as u64;
                let mut field_json_entries = Vec::new();
                let mut body = Vec::new();

                for (i, field) in fields.named.iter().enumerate() {
                    let field_ident = field.ident.as_ref().unwrap();
                    let field_name_str = field_ident.to_string();
                    let hints = parse_format_hints(&field.attrs);

                    field_json_entries.push(serde_json::json!({
                        "name": field_name_str,
                        "index": i,
                        "hints": hints,
                    }));

                    let field_json = serde_json::json!({
                        "kind": "field",
                        "type": format!("{}::{}", name_str, variant_name_str),
                        "name": field_name_str,
                        "index": i,
                    });
                    let field_hash: u64 = emit_meta(
                        "field",
                        &format!("field:{}::{}.{}", name_str, variant_name_str, field_name_str),
                        field_json,
                    );

                    body.push(quote! {
                        __f.write_field_id(#field_hash);
                        rcard_log::formatter::Format::format(#field_ident, __f);
                    });
                }

                let variant_json = serde_json::json!({
                    "kind": "variant",
                    "enum": name_str,
                    "name": variant_name_str,
                    "style": "struct",
                    "fields": field_json_entries,
                });
                let variant_hash: u64 = emit_meta("variant", &variant_key, variant_json);

                let field_idents: Vec<_> = fields.named.iter().map(|f| &f.ident).collect();

                match_arms.push(quote! {
                    #name::#variant_ident { #(#field_idents),* } => {
                        __f.with_struct(
                            #variant_hash,
                            #field_count,
                            |__f| {
                                #(#body)*
                            },
                        );
                    }
                });
            }

            Fields::Unnamed(fields) => {
                let field_count = fields.unnamed.len() as u64;

                let variant_json = serde_json::json!({
                    "kind": "variant",
                    "enum": name_str,
                    "name": variant_name_str,
                    "style": "tuple",
                    "field_count": field_count,
                });
                let variant_hash: u64 = emit_meta("variant", &variant_key, variant_json);

                let binding_names: Vec<_> = (0..fields.unnamed.len())
                    .map(|i| format_ident!("__f{}", i))
                    .collect();

                let body: Vec<_> = binding_names
                    .iter()
                    .map(|b| {
                        quote! {
                            rcard_log::formatter::Format::format(#b, __f);
                        }
                    })
                    .collect();

                match_arms.push(quote! {
                    #name::#variant_ident(#(#binding_names),*) => {
                        __f.with_tuple(
                            #variant_hash,
                            #field_count,
                            |__f| {
                                #(#body)*
                            },
                        );
                    }
                });
            }
        }
    }

    quote! {
        impl rcard_log::formatter::Format for #name {
            fn format<W: rcard_log::formatter::Writer>(&self, __f: &mut rcard_log::formatter::Formatter<W>) {
                match self {
                    #(#match_arms)*
                }
            }
        }
    }
}
