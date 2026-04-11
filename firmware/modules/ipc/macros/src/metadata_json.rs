//! Build `serde_json::Value`s describing a parsed `#[ipc::resource]` or
//! `#[ipc::interface]` trait, plus helpers for server-site records.
//!
//! The shape is consumed by `host/tfw/src/ipc_metadata.rs`. If you add a
//! field here, update the scraper's types to match.

use quote::quote;
use serde_json::{json, Value};
use syn::Ident;

use crate::parse::{ConstructorReturn, HandleMode, MethodKind, ParsedMethod, ResourceAttr};

/// Build the full JSON record for a resource-style trait
/// (`#[ipc::resource]` has `arena_size`, `#[ipc::interface]` does not).
pub fn resource_record(
    trait_name: &Ident,
    methods: &[ParsedMethod],
    attrs: &ResourceAttr,
    is_interface: bool,
) -> Value {
    let kind_label = if is_interface { "interface" } else { "resource" };

    let implements = attrs
        .implements
        .as_ref()
        .map(|p| path_to_string(p))
        .map(Value::String)
        .unwrap_or(Value::Null);

    let clone = match attrs.clone_mode {
        Some(crate::parse::CloneMode::Refcount) => json!("refcount"),
        None => Value::Null,
    };

    let methods_json: Vec<Value> = methods.iter().map(method_record).collect();

    json!({
        "type": kind_label,
        "name": trait_name.to_string(),
        "kind": attrs.kind,
        "arena_size": attrs.arena_size,
        "clone": clone,
        "implements": implements,
        "methods": methods_json,
    })
}

fn method_record(m: &ParsedMethod) -> Value {
    let params: Vec<Value> = m
        .params
        .iter()
        .map(|p| {
            let handle_mode = match p.handle_mode {
                Some(HandleMode::Move) => json!("move"),
                Some(HandleMode::Clone) => json!("clone"),
                None => Value::Null,
            };
            json!({
                "name": p.name.to_string(),
                "type": type_to_string(&p.ty),
                "is_lease": p.is_lease,
                "lease_mutable": p.lease_mutable,
                "handle_mode": handle_mode,
                "impl_trait": p.impl_trait_name.as_ref().map(|i| i.to_string()),
            })
        })
        .collect();

    let return_type = m
        .return_type
        .as_ref()
        .map(type_to_string)
        .map(Value::String)
        .unwrap_or(Value::Null);

    let ctor_return = match &m.ctor_return {
        Some(ConstructorReturn::Bare) => json!({ "kind": "self" }),
        Some(ConstructorReturn::OptionSelf) => json!({ "kind": "option_self" }),
        Some(ConstructorReturn::Result(err_ty)) => json!({
            "kind": "result",
            "error_type": type_to_string(err_ty),
        }),
        None => Value::Null,
    };

    let kind_label = match m.kind {
        MethodKind::Constructor => "constructor",
        MethodKind::Message => "message",
        MethodKind::StaticMessage => "static_message",
        MethodKind::Destructor => "destructor",
    };

    let constructs = m.constructs.as_ref().map(|(trait_name, generic)| {
        json!({
            "trait": trait_name.to_string(),
            "generic": generic.to_string(),
        })
    });

    json!({
        "id": m.method_id,
        "kind": kind_label,
        "name": m.name.to_string(),
        "params": params,
        "return_type": return_type,
        "ctor_return": ctor_return,
        "constructs": constructs,
    })
}

fn type_to_string(ty: &syn::Type) -> String {
    quote!(#ty).to_string()
}

fn path_to_string(p: &syn::Path) -> String {
    quote!(#p).to_string()
}

/// Build a server record: `{ task, serves: [trait names] }`. Emitted
/// from `ipc::server!` expansions so the host can map tasks → resources.
pub fn server_record(task_name: &str, serves: &[String]) -> Value {
    json!({
        "type": "server",
        "task": task_name,
        "serves": serves,
    })
}
