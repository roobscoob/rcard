//! Encode an `IpcValue` into postcard wire bytes using a schema for
//! validation and enum variant resolution.

use crate::value::IpcValue;
use postcard_schema::schema::owned::{OwnedDataModelType, OwnedNamedType};
use serde::ser::{self, Serialize, SerializeTupleVariant};

/// Encode a single `IpcValue` into postcard bytes, guided by a schema.
///
/// The schema is used to:
/// - Resolve enum variant names to their variant index (position).
/// - Validate that the value's shape matches what the firmware expects.
///
/// Returns the encoded bytes.
pub fn encode(schema: &OwnedNamedType, value: &IpcValue) -> Result<Vec<u8>, EncodeError> {
    // Resolve enum variants in the value tree against the schema,
    // then serialize via postcard.
    let resolved = resolve_enums(schema, value)?;
    postcard::to_allocvec(&resolved).map_err(|e| EncodeError::Postcard(e.to_string()))
}

/// Encode a tuple of values (one per method parameter) into postcard bytes.
pub fn encode_args(
    schemas: &[(&str, &OwnedNamedType)],
    value: &IpcValue,
) -> Result<Vec<u8>, EncodeError> {
    encode_args_with_handle(schemas, None, value)
}

/// Like `encode_args`, but prepends a `RawHandle` (u64) as the first
/// tuple element when the method is `Message` or `Destructor`. Matches
/// the server-side deserialize tuple shape `(RawHandle, arg1, arg2...)`.
pub fn encode_args_with_handle(
    schemas: &[(&str, &OwnedNamedType)],
    handle: Option<u64>,
    value: &IpcValue,
) -> Result<Vec<u8>, EncodeError> {
    let IpcValue::Struct(fields) = value else {
        return Err(EncodeError::ExpectedStruct);
    };

    let mut resolved_items = Vec::with_capacity(schemas.len() + 1);
    if let Some(h) = handle {
        resolved_items.push(IpcValue::U64(h));
    }
    for (param_name, param_schema) in schemas {
        let field_value = fields
            .get(*param_name)
            .ok_or_else(|| EncodeError::MissingField(param_name.to_string()))?;
        resolved_items.push(resolve_enums(param_schema, field_value)?);
    }

    let tuple = IpcValue::Tuple(resolved_items);
    postcard::to_allocvec(&tuple).map_err(|e| EncodeError::Postcard(e.to_string()))
}

/// Walk an `IpcValue` tree and resolve enum variant names to variant
/// indices using the schema. Returns a new tree with `Enum` nodes
/// replaced by `ResolvedEnum` wrapper values that serialize with the
/// correct variant index.
fn resolve_enums(schema: &OwnedNamedType, value: &IpcValue) -> Result<IpcValue, EncodeError> {
    match (&schema.ty, value) {
        // Enum: look up variant name → index
        (OwnedDataModelType::Enum(variants), IpcValue::Enum { variant, payload }) => {
            let (idx, _variant_def) = variants
                .iter()
                .enumerate()
                .find(|(_, v)| v.name == *variant)
                .ok_or_else(|| {
                    EncodeError::UnknownVariant {
                        enum_name: schema.name.clone(),
                        variant: variant.clone(),
                        available: variants.iter().map(|v| v.name.clone()).collect(),
                    }
                })?;

            Ok(IpcValue::Enum {
                variant: format!("{}:{}", idx, variant),
                payload: payload.clone(),
            })
        }

        // Option<T>: recurse into inner
        (OwnedDataModelType::Option(inner_schema), IpcValue::Some(inner)) => {
            let resolved = resolve_enums(inner_schema, inner)?;
            Ok(IpcValue::Some(Box::new(resolved)))
        }
        (OwnedDataModelType::Option(_), IpcValue::None) => Ok(IpcValue::None),
        // Allow passing a bare value where Option is expected → wrap in Some
        (OwnedDataModelType::Option(inner_schema), other) => {
            let resolved = resolve_enums(inner_schema, other)?;
            Ok(IpcValue::Some(Box::new(resolved)))
        }

        // Struct: recurse into fields
        (OwnedDataModelType::Struct(field_schemas), IpcValue::Struct(fields)) => {
            let mut resolved = indexmap::IndexMap::new();
            for schema_field in field_schemas {
                let field_value = fields
                    .get(&schema_field.name)
                    .ok_or_else(|| EncodeError::MissingField(schema_field.name.clone()))?;
                resolved.insert(
                    schema_field.name.clone(),
                    resolve_enums(&schema_field.ty, field_value)?,
                );
            }
            Ok(IpcValue::Struct(resolved))
        }

        // Tuple: recurse into elements
        (OwnedDataModelType::Tuple(element_schemas), IpcValue::Tuple(items)) => {
            let resolved: Result<Vec<_>, _> = element_schemas
                .iter()
                .zip(items.iter())
                .map(|(s, v)| resolve_enums(s, v))
                .collect();
            Ok(IpcValue::Tuple(resolved?))
        }

        // Primitives and other leaves: pass through unchanged
        _ => Ok(value.clone()),
    }
}

/// Errors from encoding.
#[derive(Debug)]
pub enum EncodeError {
    /// The user value's shape doesn't match the schema.
    ExpectedStruct,
    /// A required field is missing from the user's struct.
    MissingField(String),
    /// An enum variant name wasn't found in the schema.
    UnknownVariant {
        enum_name: String,
        variant: String,
        available: Vec<String>,
    },
    /// Postcard serialization failed.
    Postcard(String),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExpectedStruct => write!(f, "expected IpcValue::Struct for method args"),
            Self::MissingField(name) => write!(f, "missing field: {name}"),
            Self::UnknownVariant {
                enum_name,
                variant,
                available,
            } => write!(
                f,
                "unknown variant '{variant}' for enum {enum_name}; available: {available:?}"
            ),
            Self::Postcard(e) => write!(f, "postcard encode: {e}"),
        }
    }
}

impl std::error::Error for EncodeError {}
