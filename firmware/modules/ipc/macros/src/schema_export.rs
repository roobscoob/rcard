//! Generate the `__ipc_schema` module for a resource trait.
//!
//! This module is always compiled (no cfg gate) so both firmware and
//! host builds can access it. It contains a single `RESOURCE` const
//! of type `ipc::ResourceDesc` that describes the resource name, kind,
//! and every method's parameters + return type via postcard-schema's
//! `NamedType` references. The schema-dump tool iterates these to
//! build `ipc-metadata.json`.

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::Ident;

use crate::parse::{interface_op_path, MethodKind, ParsedMethod, ResourceAttr};
use crate::util::to_pascal_case;
use crate::wire_format::types::wire_type_for;

pub fn gen_schema_export(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    attrs: &ResourceAttr,
) -> TokenStream2 {
    let resource_name = trait_name.to_string();
    let kind = attrs.kind;

    // When a resource `implements(iface)`, method IDs are remapped at
    // runtime (see `op_enum.rs`): Message/StaticMessage methods inherit
    // the interface's ID by name, while Constructor/Destructor methods
    // sit past the interface's METHOD_COUNT in declaration order. The
    // schema dump must emit these *runtime* IDs so the host can target
    // methods with the correct opcode.
    let iface_op = attrs.implements.as_ref().map(interface_op_path);
    let mut non_message_offset: u8 = 0;

    let method_ids: Vec<TokenStream2> = methods
        .iter()
        .map(|m| {
            let fallback = m.method_id;
            if let Some(ref iface_op) = iface_op {
                let variant = format_ident!("{}", to_pascal_case(&m.name.to_string()));
                if m.kind == MethodKind::Message || m.kind == MethodKind::StaticMessage {
                    quote! { #iface_op::#variant as u8 }
                } else {
                    let offset = proc_macro2::Literal::u8_suffixed(non_message_offset);
                    non_message_offset += 1;
                    quote! { #iface_op::METHOD_COUNT + #offset }
                }
            } else {
                quote! { #fallback }
            }
        })
        .collect();

    let method_descs: Vec<TokenStream2> = methods
        .iter()
        .zip(method_ids.iter())
        .map(|(m, id)| {
            let name = m.name.to_string();
            let kind_str = match m.kind {
                MethodKind::Constructor => "constructor",
                MethodKind::Message => "message",
                MethodKind::StaticMessage => "static_message",
                MethodKind::Destructor => "destructor",
            };

            // Non-lease, non-handle params — these get a schema reference.
            let param_descs: Vec<TokenStream2> = m
                .params
                .iter()
                .filter(|p| !p.is_lease && p.handle_mode.is_none())
                .map(|p| {
                    let pname = p.name.to_string();
                    let ty = wire_type_for(p);
                    quote! {
                        ipc::ParamDesc {
                            name: #pname,
                            schema: <#ty as ipc::__postcard_schema::Schema>::SCHEMA,
                        }
                    }
                })
                .collect();

            // Lease params — just name + direction, no schema.
            let lease_descs: Vec<TokenStream2> = m
                .params
                .iter()
                .filter(|p| p.is_lease)
                .map(|p| {
                    let pname = p.name.to_string();
                    let mutable = p.lease_mutable;
                    quote! {
                        ipc::LeaseParamDesc {
                            name: #pname,
                            mutable: #mutable,
                        }
                    }
                })
                .collect();

            // Return type schema. On the wire, `Self` (constructors) and
            // `constructs` generic idents both map to `RawHandle`. Replace
            // them so the schema reference resolves in a const context.
            let return_schema = if let Some(rt) = &m.return_type {
                let mut wire_rt = rt.clone();
                // Constructor: Self → RawHandle
                if m.kind == MethodKind::Constructor {
                    let replaced = crate::util::replace_ident_in_type(
                        &wire_rt,
                        &syn::Ident::new("Self", proc_macro2::Span::call_site()),
                        &quote! { ipc::RawHandle },
                    );
                    wire_rt = syn::parse2(replaced)
                        .expect("ipc: failed to parse replaced constructor return type");
                }
                // constructs(Trait = G): G → RawHandle
                if let Some((_trait_name, generic_ident)) = &m.constructs {
                    let replaced = crate::util::replace_ident_in_type(
                        &wire_rt,
                        generic_ident,
                        &quote! { ipc::RawHandle },
                    );
                    wire_rt = syn::parse2(replaced)
                        .expect("ipc: failed to parse replaced constructs return type");
                }
                quote! { Some(<#wire_rt as ipc::__postcard_schema::Schema>::SCHEMA) }
            } else {
                quote! { None }
            };

            quote! {
                ipc::MethodDesc {
                    name: #name,
                    id: #id,
                    kind: #kind_str,
                    params: &[ #( #param_descs ),* ],
                    lease_params: &[ #( #lease_descs ),* ],
                    return_schema: #return_schema,
                }
            }
        })
        .collect();

    let mod_name = quote::format_ident!("__ipc_schema_{}", trait_name.to_string().to_lowercase());

    quote! {
        #[doc(hidden)]
        pub mod #mod_name {
            use super::*;
            pub const RESOURCE: ipc::ResourceDesc = ipc::ResourceDesc {
                name: #resource_name,
                kind: #kind,
                methods: &[ #( #method_descs ),* ],
            };
        }
    }
}
