//! The `Registry` loads method schemas from ipc-metadata and provides
//! the top-level `encode_call` / `decode_reply` API.

use std::collections::HashMap;

use postcard_schema::schema::owned::OwnedNamedType;
use rcard_usb_proto::{LeaseDescriptor, LeaseKind};

use crate::encode::{self, EncodeError};
use crate::decode::{self, DecodeError};
use crate::value::IpcValue;

/// A method's full schema, loaded from ipc-metadata.json.
#[derive(Debug, Clone)]
pub struct MethodSchema {
    pub resource_name: String,
    pub method_name: String,
    pub method_id: u8,
    pub resource_kind: u8,
    /// Non-lease parameter schemas, in declaration order.
    pub params: Vec<(String, OwnedNamedType)>,
    /// Lease parameter descriptors (name + direction).
    pub leases: Vec<LeaseInfo>,
    /// Return type schema (None for void methods).
    pub return_schema: Option<OwnedNamedType>,
}

#[derive(Debug, Clone)]
pub struct LeaseInfo {
    pub name: String,
    pub mutable: bool,
}

/// The result of encoding a method call.
pub struct EncodedCall {
    /// Target task index — resolved separately from server metadata.
    pub task_id: u16,
    pub resource_kind: u8,
    pub method_id: u8,
    /// Postcard-encoded non-lease arguments.
    pub wire_args: Vec<u8>,
    /// Lease descriptors for the IpcRequest.
    pub leases: Vec<LeaseDescriptor>,
    /// Lease data slices, one per Read/ReadWrite lease.
    pub lease_data: Vec<Vec<u8>>,
}

/// Loaded schema registry — provides encode/decode for any method
/// the firmware exposes.
pub struct Registry {
    methods: HashMap<(String, String), MethodSchema>,
    /// Maps resource name → task_id (from server metadata).
    resource_task_ids: HashMap<String, u16>,
}

impl Registry {
    /// Build a registry from the schema JSON emitted by the schema dump.
    ///
    /// `schemas_json` is the `schemas` field from `IpcMetadataBundle`,
    /// which is an array of resource descriptors each containing methods
    /// with their param/return schemas.
    ///
    /// `servers_json` is an optional map of server entries (task → resources
    /// + task_id). Used to resolve resource names to task_ids for routing.
    pub fn from_schemas_json(
        json: &serde_json::Value,
        servers_json: Option<&serde_json::Value>,
    ) -> Result<Self, RegistryError> {
        let mut methods = HashMap::new();
        let mut resource_task_ids = HashMap::new();

        // Build the resource → task_id map from server metadata.
        if let Some(servers) = servers_json {
            if let Some(obj) = servers.as_object() {
                for (_task_name, entry) in obj {
                    let task_id = entry
                        .get("task_id")
                        .and_then(|v| v.as_u64())
                        .map(|v| v as u16);
                    if let (Some(serves), Some(tid)) = (entry["serves"].as_array(), task_id) {
                        for resource_val in serves {
                            if let Some(name) = resource_val.as_str() {
                                resource_task_ids.insert(name.to_string(), tid);
                            }
                        }
                    }
                }
            }
        }

        let resources = json
            .as_array()
            .ok_or(RegistryError::BadShape("expected array of resources"))?;

        for resource in resources {
            let resource_name = resource["name"]
                .as_str()
                .ok_or(RegistryError::BadShape("resource missing name"))?;
            let resource_kind = resource["kind"]
                .as_u64()
                .ok_or(RegistryError::BadShape("resource missing kind"))? as u8;

            let methods_arr = resource["methods"]
                .as_array()
                .ok_or(RegistryError::BadShape("resource missing methods"))?;

            for method in methods_arr {
                let method_name = method["name"]
                    .as_str()
                    .ok_or(RegistryError::BadShape("method missing name"))?;
                let method_id = method["id"]
                    .as_u64()
                    .ok_or(RegistryError::BadShape("method missing id"))? as u8;

                let mut params = Vec::new();
                if let Some(params_arr) = method["params"].as_array() {
                    for p in params_arr {
                        let name = p["name"]
                            .as_str()
                            .ok_or(RegistryError::BadShape("param missing name"))?
                            .to_string();
                        let schema: OwnedNamedType =
                            serde_json::from_value(p["schema"].clone())
                                .map_err(|e| RegistryError::SchemaParseError(e.to_string()))?;
                        params.push((name, schema));
                    }
                }

                let mut leases = Vec::new();
                if let Some(leases_arr) = method["lease_params"].as_array() {
                    for l in leases_arr {
                        leases.push(LeaseInfo {
                            name: l["name"]
                                .as_str()
                                .ok_or(RegistryError::BadShape("lease missing name"))?
                                .to_string(),
                            mutable: l["mutable"].as_bool().unwrap_or(false),
                        });
                    }
                }

                let return_schema = method
                    .get("return_schema")
                    .and_then(|v| if v.is_null() { None } else { Some(v) })
                    .map(|v| serde_json::from_value::<OwnedNamedType>(v.clone()))
                    .transpose()
                    .map_err(|e| RegistryError::SchemaParseError(e.to_string()))?;

                let schema = MethodSchema {
                    resource_name: resource_name.to_string(),
                    method_name: method_name.to_string(),
                    method_id,
                    resource_kind,
                    params,
                    leases,
                    return_schema,
                };

                methods.insert(
                    (resource_name.to_string(), method_name.to_string()),
                    schema,
                );
            }
        }

        Ok(Registry {
            methods,
            resource_task_ids,
        })
    }

    /// Look up a method's schema by resource + method name.
    pub fn method(&self, resource: &str, method: &str) -> Option<&MethodSchema> {
        self.methods.get(&(resource.to_string(), method.to_string()))
    }

    /// Encode a method call from user-supplied args.
    ///
    /// `value` should be an `IpcValue::Struct` with field names matching
    /// the method's non-lease parameter names. Lease data is extracted
    /// from fields matching the lease parameter names.
    ///
    /// `task_id` is resolved from the server metadata loaded into the
    /// registry. Returns an error if the resource has no known task_id.
    pub fn encode_call(
        &self,
        resource: &str,
        method: &str,
        value: IpcValue,
    ) -> Result<EncodedCall, CallError> {
        let task_id = self
            .resource_task_ids
            .get(resource)
            .copied()
            .unwrap_or(0); // fallback to 0 if no server metadata
        let schema = self
            .method(resource, method)
            .ok_or_else(|| CallError::UnknownMethod(resource.into(), method.into()))?;

        let IpcValue::Struct(mut fields) = value else {
            return Err(CallError::Encode(EncodeError::ExpectedStruct));
        };

        // Extract lease data from the value's fields.
        let mut lease_descriptors = Vec::new();
        let mut lease_data = Vec::new();
        for lease_info in &schema.leases {
            let data = fields
                .swap_remove(&lease_info.name)
                .and_then(|v| match v {
                    IpcValue::Bytes(b) => Some(b),
                    _ => None,
                })
                .unwrap_or_default();

            let kind = if lease_info.mutable {
                LeaseKind::ReadWrite
            } else {
                LeaseKind::Read
            };
            lease_descriptors.push(LeaseDescriptor {
                kind,
                length: data.len() as u16,
            });
            lease_data.push(data);
        }

        // Remaining fields are the non-lease args. Encode via postcard.
        let param_schemas: Vec<(&str, &OwnedNamedType)> = schema
            .params
            .iter()
            .map(|(name, schema)| (name.as_str(), schema))
            .collect();

        let args_value = IpcValue::Struct(fields);
        let wire_args =
            encode::encode_args(&param_schemas, &args_value).map_err(CallError::Encode)?;

        Ok(EncodedCall {
            task_id,
            resource_kind: schema.resource_kind,
            method_id: schema.method_id,
            wire_args,
            leases: lease_descriptors,
            lease_data,
        })
    }
}

#[derive(Debug)]
pub enum RegistryError {
    BadShape(&'static str),
    SchemaParseError(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadShape(msg) => write!(f, "registry: {msg}"),
            Self::SchemaParseError(e) => write!(f, "registry schema parse: {e}"),
        }
    }
}

impl std::error::Error for RegistryError {}

#[derive(Debug)]
pub enum CallError {
    UnknownMethod(String, String),
    Encode(EncodeError),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownMethod(r, m) => write!(f, "unknown method: {r}::{m}"),
            Self::Encode(e) => write!(f, "encode: {e}"),
        }
    }
}

impl std::error::Error for CallError {}
