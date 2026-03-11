extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
pub enum OwnedValue {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    U128(u128),
    I128(i128),
    F32(f32),
    F64(f64),
    Char(char),
    Bool(bool),
    Str(String),
    Unit,
    Array(Vec<OwnedValue>),
    Slice(Vec<OwnedValue>),
    Tuple {
        type_id: u64,
        fields: Vec<OwnedValue>,
    },
    Struct {
        type_id: u64,
        fields: Vec<(u64, OwnedValue)>,
    },
}
