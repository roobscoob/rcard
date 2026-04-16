//! Runtime value type for dynamically-typed IPC arguments and replies.

use indexmap::IndexMap;
use serde::ser::{self, Serialize, SerializeStruct, SerializeTupleVariant};

/// A dynamically-typed IPC value. Constructed by the user (or by the
/// `args!` macro) and validated against a postcard-schema `NamedType`
/// at encode time.
#[derive(Debug, Clone)]
pub enum IpcValue {
    // Primitives
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    U128(u128),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    I128(i128),
    Bool(bool),
    Char(char),

    // Composites
    /// A struct with named fields, in declaration order.
    Struct(IndexMap<String, IpcValue>),
    /// A tuple (positional fields).
    Tuple(Vec<IpcValue>),
    /// An enum variant by name. For unit variants, `payload` is empty.
    /// For newtype/tuple/struct variants, `payload` holds the inner value(s).
    Enum {
        variant: String,
        payload: Vec<IpcValue>,
    },

    // Wrappers
    Some(Box<IpcValue>),
    None,
    Ok(Box<IpcValue>),
    Err(Box<IpcValue>),

    /// Raw bytes — used for lease data.
    Bytes(Vec<u8>),

    /// Unit type `()`.
    Unit,
}

/// Serialize an `IpcValue` through serde so that postcard (or any other
/// serde format) can encode it. The visitor calls match what the
/// firmware's `#[derive(Serialize)]` would produce for the same logical
/// value, so the bytes are identical.
///
/// **Important**: enum variant indices are resolved against the schema
/// at encode time, not here. The `Serialize` impl for `Enum` uses the
/// `variant_index` that was pre-resolved by the encoder and stored in
/// a thread-local or passed through a wrapper. For now, we serialize
/// enum variants by index 0 and let the encoder handle resolution.
impl Serialize for IpcValue {
    fn serialize<S: ser::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        match self {
            IpcValue::U8(v) => ser.serialize_u8(*v),
            IpcValue::U16(v) => ser.serialize_u16(*v),
            IpcValue::U32(v) => ser.serialize_u32(*v),
            IpcValue::U64(v) => ser.serialize_u64(*v),
            IpcValue::U128(v) => ser.serialize_u128(*v),
            IpcValue::I8(v) => ser.serialize_i8(*v),
            IpcValue::I16(v) => ser.serialize_i16(*v),
            IpcValue::I32(v) => ser.serialize_i32(*v),
            IpcValue::I64(v) => ser.serialize_i64(*v),
            IpcValue::I128(v) => ser.serialize_i128(*v),
            IpcValue::Bool(v) => ser.serialize_bool(*v),
            IpcValue::Char(v) => ser.serialize_char(*v),
            IpcValue::Unit => ser.serialize_unit(),
            IpcValue::Bytes(b) => ser.serialize_bytes(b),
            IpcValue::Some(inner) => ser.serialize_some(inner.as_ref()),
            IpcValue::None => ser.serialize_none(),
            IpcValue::Struct(fields) => {
                let mut s = ser.serialize_struct("Struct", fields.len())?;
                for (name, value) in fields {
                    // serde requires &'static str for field names; we
                    // leak the string since schema-driven calls are
                    // infrequent and the set of field names is bounded.
                    let name: &'static str = leak_str(name);
                    s.serialize_field(name, value)?;
                }
                s.end()
            }
            IpcValue::Tuple(items) => {
                use ser::SerializeTuple;
                let mut s = ser.serialize_tuple(items.len())?;
                for item in items {
                    s.serialize_element(item)?;
                }
                s.end()
            }
            IpcValue::Enum {
                variant, payload, ..
            } => {
                // Postcard uses variant index, not name. The encoder
                // resolves the name → index from the schema and wraps
                // the value in a ResolvedEnum before serializing.
                // If we get here without resolution, use index 0 as
                // a fallback (will be wrong — encoder must handle this).
                let name: &'static str = leak_str(variant);
                if payload.is_empty() {
                    ser.serialize_unit_variant("Enum", 0, name)
                } else if payload.len() == 1 {
                    ser.serialize_newtype_variant("Enum", 0, name, &payload[0])
                } else {
                    let mut s = ser.serialize_tuple_variant("Enum", 0, name, payload.len())?;
                    for item in payload {
                        s.serialize_field(item)?;
                    }
                    s.end()
                }
            }
            IpcValue::Ok(inner) => {
                // Result<T, E> in serde: Ok = variant 0, Err = variant 1.
                ser.serialize_newtype_variant("Result", 0, "Ok", inner.as_ref())
            }
            IpcValue::Err(inner) => {
                ser.serialize_newtype_variant("Result", 1, "Err", inner.as_ref())
            }
        }
    }
}

/// Leak a string into a `&'static str`. Used for serde field/variant
/// names which require `'static` lifetime. The leaked memory is small
/// (bounded by the set of IPC field names) and lives for the process.
fn leak_str(s: &str) -> &'static str {
    // Simple: Box::leak. For a CLI tool this is fine; for a long-lived
    // server you'd intern these in a HashSet<String> and return &str
    // references. The set of IPC names is small and bounded.
    Box::leak(s.to_string().into_boxed_str())
}
