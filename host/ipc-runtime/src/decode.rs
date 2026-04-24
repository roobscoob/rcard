//! Decode postcard wire bytes into an `IpcValue`, guided by an
//! `OwnedNamedType` schema.
//!
//! Postcard is a non-self-describing binary format; decoding requires
//! the schema as out-of-band context. We walk the schema recursively via
//! serde's `DeserializeSeed` pattern, dispatching to the matching
//! `deserialize_*` call on postcard's `Deserializer` at each node.

use indexmap::IndexMap;
use postcard_schema::schema::owned::{
    OwnedDataModelType, OwnedDataModelVariant, OwnedNamedType, OwnedNamedValue, OwnedNamedVariant,
};
use serde::de::{
    DeserializeSeed, EnumAccess, Error as DeError, MapAccess, SeqAccess, VariantAccess, Visitor,
};

use crate::value::IpcValue;

/// Decode a postcard-encoded reply into an `IpcValue` guided by the
/// return type's schema.
pub fn decode(schema: &OwnedNamedType, bytes: &[u8]) -> Result<IpcValue, DecodeError> {
    let mut deserializer = postcard::Deserializer::from_bytes(bytes);
    let value = ValueSeed(schema)
        .deserialize(&mut deserializer)
        .map_err(|e| DecodeError::Postcard(e.to_string()))?;
    Ok(canonicalize_result(value))
}

/// Rewrite `Enum { variant: "Ok"|"Err", payload: [inner] }` (our default
/// shape for generic enums) into `IpcValue::Ok(inner)` / `Err(inner)` so
/// the decoded representation matches what the encoder produces for
/// `Result<T, E>`. Applied recursively, bottom-up.
fn canonicalize_result(v: IpcValue) -> IpcValue {
    match v {
        IpcValue::Enum { variant, payload } if payload.len() == 1 && (variant == "Ok" || variant == "Err") => {
            let inner = canonicalize_result(payload.into_iter().next().unwrap());
            if variant == "Ok" {
                IpcValue::Ok(Box::new(inner))
            } else {
                IpcValue::Err(Box::new(inner))
            }
        }
        IpcValue::Enum { variant, payload } => IpcValue::Enum {
            variant,
            payload: payload.into_iter().map(canonicalize_result).collect(),
        },
        IpcValue::Struct(fields) => IpcValue::Struct(
            fields
                .into_iter()
                .map(|(k, v)| (k, canonicalize_result(v)))
                .collect(),
        ),
        IpcValue::Tuple(items) => {
            IpcValue::Tuple(items.into_iter().map(canonicalize_result).collect())
        }
        IpcValue::Some(inner) => IpcValue::Some(Box::new(canonicalize_result(*inner))),
        IpcValue::Ok(inner) => IpcValue::Ok(Box::new(canonicalize_result(*inner))),
        IpcValue::Err(inner) => IpcValue::Err(Box::new(canonicalize_result(*inner))),
        other => other,
    }
}

// ── Seeds ───────────────────────────────────────────────────────────

struct ValueSeed<'a>(&'a OwnedNamedType);

impl<'de, 'a> DeserializeSeed<'de> for ValueSeed<'a> {
    type Value = IpcValue;

    fn deserialize<D>(self, d: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use OwnedDataModelType as T;
        match &self.0.ty {
            T::Bool => d.deserialize_bool(PrimVisitor),
            T::I8 => d.deserialize_i8(PrimVisitor),
            T::I16 => d.deserialize_i16(PrimVisitor),
            T::I32 => d.deserialize_i32(PrimVisitor),
            T::I64 => d.deserialize_i64(PrimVisitor),
            T::I128 => d.deserialize_i128(PrimVisitor),
            T::U8 => d.deserialize_u8(PrimVisitor),
            T::U16 => d.deserialize_u16(PrimVisitor),
            T::U32 => d.deserialize_u32(PrimVisitor),
            T::U64 => d.deserialize_u64(PrimVisitor),
            T::U128 => d.deserialize_u128(PrimVisitor),
            T::Usize => d.deserialize_u64(PrimVisitor),
            T::Isize => d.deserialize_i64(PrimVisitor),
            T::F32 | T::F64 => Err(D::Error::custom("float types not supported by IpcValue")),
            T::Char => d.deserialize_char(PrimVisitor),
            T::String => Err(D::Error::custom("String not supported by IpcValue")),
            T::ByteArray => d.deserialize_byte_buf(PrimVisitor),
            T::Option(inner) => d.deserialize_option(OptionSeed(inner)),
            T::Unit | T::UnitStruct => d.deserialize_unit(PrimVisitor),
            T::NewtypeStruct(inner) => {
                // Transparent on the wire — delegate to inner.
                ValueSeed(inner).deserialize(d)
            }
            T::Seq(inner) => d.deserialize_seq(SeqSeed(inner)),
            T::Tuple(elems) | T::TupleStruct(elems) => {
                d.deserialize_tuple(elems.len(), TupleSeed(elems))
            }
            T::Map { key, val } => d.deserialize_map(MapSeed(key, val)),
            T::Struct(fields) => d.deserialize_tuple(fields.len(), StructSeed(fields)),
            T::Enum(variants) => d.deserialize_enum("", &[], EnumSeed(variants)),
            T::Schema => Err(D::Error::custom("Schema leaf not decodable")),
        }
    }
}

struct PrimVisitor;

impl<'de> Visitor<'de> for PrimVisitor {
    type Value = IpcValue;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "an IPC primitive")
    }

    fn visit_bool<E: DeError>(self, v: bool) -> Result<Self::Value, E> {
        Ok(IpcValue::Bool(v))
    }
    fn visit_i8<E: DeError>(self, v: i8) -> Result<Self::Value, E> {
        Ok(IpcValue::I8(v))
    }
    fn visit_i16<E: DeError>(self, v: i16) -> Result<Self::Value, E> {
        Ok(IpcValue::I16(v))
    }
    fn visit_i32<E: DeError>(self, v: i32) -> Result<Self::Value, E> {
        Ok(IpcValue::I32(v))
    }
    fn visit_i64<E: DeError>(self, v: i64) -> Result<Self::Value, E> {
        Ok(IpcValue::I64(v))
    }
    fn visit_i128<E: DeError>(self, v: i128) -> Result<Self::Value, E> {
        Ok(IpcValue::I128(v))
    }
    fn visit_u8<E: DeError>(self, v: u8) -> Result<Self::Value, E> {
        Ok(IpcValue::U8(v))
    }
    fn visit_u16<E: DeError>(self, v: u16) -> Result<Self::Value, E> {
        Ok(IpcValue::U16(v))
    }
    fn visit_u32<E: DeError>(self, v: u32) -> Result<Self::Value, E> {
        Ok(IpcValue::U32(v))
    }
    fn visit_u64<E: DeError>(self, v: u64) -> Result<Self::Value, E> {
        Ok(IpcValue::U64(v))
    }
    fn visit_u128<E: DeError>(self, v: u128) -> Result<Self::Value, E> {
        Ok(IpcValue::U128(v))
    }
    fn visit_char<E: DeError>(self, v: char) -> Result<Self::Value, E> {
        Ok(IpcValue::Char(v))
    }
    fn visit_bytes<E: DeError>(self, v: &[u8]) -> Result<Self::Value, E> {
        Ok(IpcValue::Bytes(v.to_vec()))
    }
    fn visit_byte_buf<E: DeError>(self, v: Vec<u8>) -> Result<Self::Value, E> {
        Ok(IpcValue::Bytes(v))
    }
    fn visit_unit<E: DeError>(self) -> Result<Self::Value, E> {
        Ok(IpcValue::Unit)
    }
}

struct OptionSeed<'a>(&'a OwnedNamedType);

impl<'de, 'a> Visitor<'de> for OptionSeed<'a> {
    type Value = IpcValue;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "an Option")
    }
    fn visit_none<E: DeError>(self) -> Result<Self::Value, E> {
        Ok(IpcValue::None)
    }
    fn visit_some<D>(self, d: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let inner = ValueSeed(self.0).deserialize(d)?;
        Ok(IpcValue::Some(Box::new(inner)))
    }
}

struct SeqSeed<'a>(&'a OwnedNamedType);

impl<'de, 'a> Visitor<'de> for SeqSeed<'a> {
    type Value = IpcValue;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "a sequence")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(ValueSeed(self.0))? {
            items.push(item);
        }
        Ok(IpcValue::Tuple(items))
    }
}

struct TupleSeed<'a>(&'a [OwnedNamedType]);

impl<'de, 'a> Visitor<'de> for TupleSeed<'a> {
    type Value = IpcValue;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "a tuple of {} elements", self.0.len())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::with_capacity(self.0.len());
        for elem_schema in self.0 {
            let item = seq
                .next_element_seed(ValueSeed(elem_schema))?
                .ok_or_else(|| A::Error::custom("tuple length mismatch"))?;
            items.push(item);
        }
        Ok(IpcValue::Tuple(items))
    }
}

struct StructSeed<'a>(&'a [OwnedNamedValue]);

impl<'de, 'a> Visitor<'de> for StructSeed<'a> {
    type Value = IpcValue;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "a struct of {} fields", self.0.len())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut fields = IndexMap::new();
        for field in self.0 {
            let value = seq
                .next_element_seed(ValueSeed(&field.ty))?
                .ok_or_else(|| A::Error::custom("struct field missing"))?;
            fields.insert(field.name.clone(), value);
        }
        Ok(IpcValue::Struct(fields))
    }
}

struct MapSeed<'a>(&'a OwnedNamedType, &'a OwnedNamedType);

impl<'de, 'a> Visitor<'de> for MapSeed<'a> {
    type Value = IpcValue;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "a map")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut items = Vec::new();
        while let Some(key) = map.next_key_seed(ValueSeed(self.0))? {
            let val = map.next_value_seed(ValueSeed(self.1))?;
            items.push(IpcValue::Tuple(vec![key, val]));
        }
        Ok(IpcValue::Tuple(items))
    }
}

struct EnumSeed<'a>(&'a [OwnedNamedVariant]);

impl<'de, 'a> Visitor<'de> for EnumSeed<'a> {
    type Value = IpcValue;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "an enum")
    }
    fn visit_enum<A: EnumAccess<'de>>(self, access: A) -> Result<Self::Value, A::Error> {
        let (variant_index, variant) = access.variant_seed(VariantIndexSeed)?;
        let variant_def = self
            .0
            .get(variant_index as usize)
            .ok_or_else(|| A::Error::custom(format!("unknown variant index {variant_index}")))?;
        let name = variant_def.name.clone();
        let payload = match &variant_def.ty {
            OwnedDataModelVariant::UnitVariant => {
                variant.unit_variant()?;
                vec![]
            }
            OwnedDataModelVariant::NewtypeVariant(inner) => {
                let v = variant.newtype_variant_seed(ValueSeed(inner))?;
                vec![v]
            }
            OwnedDataModelVariant::TupleVariant(elems) => {
                let tuple = variant.tuple_variant(elems.len(), TupleSeed(elems))?;
                match tuple {
                    IpcValue::Tuple(t) => t,
                    other => vec![other],
                }
            }
            OwnedDataModelVariant::StructVariant(fields) => {
                let s = variant.struct_variant(&[], StructSeed(fields))?;
                vec![s]
            }
        };
        Ok(IpcValue::Enum {
            variant: name,
            payload,
        })
    }
}

/// Reads postcard's varint u32 variant index via `IntoDeserializer`.
struct VariantIndexSeed;

impl<'de> DeserializeSeed<'de> for VariantIndexSeed {
    type Value = u32;
    fn deserialize<D>(self, d: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // postcard hands us an `u32 IntoDeserializer` via variant_seed.
        d.deserialize_u32(VariantIndexVisitor)
    }
}

struct VariantIndexVisitor;

impl<'de> Visitor<'de> for VariantIndexVisitor {
    type Value = u32;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "a variant index")
    }
    fn visit_u32<E: DeError>(self, v: u32) -> Result<u32, E> {
        Ok(v)
    }
}

#[derive(Debug)]
pub enum DecodeError {
    Postcard(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Postcard(e) => write!(f, "postcard decode: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}
