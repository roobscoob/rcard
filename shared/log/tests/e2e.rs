extern crate alloc;

use rcard_log::decoder::{DecodeError, Decoder, FeedResult};
use rcard_log::formatter::{Format, Formatter, SliceWriter, Writer};
use rcard_log::OwnedValue;

// Provide noop extern fns for LogWriter in test binary
#[no_mangle]
fn __rcard_log_send(_level: u8, _species: u64, _data: &[u8]) {}

#[no_mangle]
fn __rcard_log_start(_level: u8, _species: u64) -> Option<u64> {
    Some(0)
}

#[no_mangle]
fn __rcard_log_write(_handle: u64, _data: &[u8]) {}

#[no_mangle]
fn __rcard_log_end(_handle: u64) {}

// --- Helper: encode via Formatter, decode via Decoder, return OwnedValue ---

struct VecWriter(Vec<u8>);

impl Writer for VecWriter {
    fn write(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

fn encode<T: Format + ?Sized>(value: &T) -> Vec<u8> {
    let mut f = Formatter::new(VecWriter(Vec::new()));
    value.format(&mut f);
    f.into_inner().0
}

fn decode_all(bytes: &[u8]) -> OwnedValue {
    let mut decoder = Decoder::new();
    let (consumed, result) = decoder.feed(bytes);
    match result {
        FeedResult::Done(v) => {
            assert_eq!(consumed, bytes.len(), "decoder didn't consume all bytes");
            v
        }
        FeedResult::Incomplete => panic!("decoder returned Incomplete for {} bytes", bytes.len()),
        FeedResult::Error(e) => panic!("decoder returned error: {:?}", e),
    }
}

fn roundtrip<T: Format + ?Sized>(value: &T) -> OwnedValue {
    decode_all(&encode(value))
}

// Also test byte-at-a-time feeding to stress the streaming state machine
fn decode_byte_at_a_time(bytes: &[u8]) -> OwnedValue {
    let mut decoder = Decoder::new();
    for (i, &byte) in bytes.iter().enumerate() {
        let (consumed, result) = decoder.feed(&[byte]);
        assert_eq!(consumed, 1);
        match result {
            FeedResult::Incomplete => {
                if i == bytes.len() - 1 {
                    panic!("decoder still Incomplete after all {} bytes", bytes.len());
                }
            }
            FeedResult::Done(v) => {
                assert_eq!(i, bytes.len() - 1, "decoder finished early at byte {}", i);
                return v;
            }
            FeedResult::Error(e) => panic!("decoder error at byte {}: {:?}", i, e),
        }
    }
    unreachable!()
}

fn roundtrip_streamed<T: Format + ?Sized>(value: &T) -> OwnedValue {
    decode_byte_at_a_time(&encode(value))
}

// ===================== Primitive Types =====================

#[test]
fn unit() {
    assert_eq!(roundtrip(&()), OwnedValue::Unit);
}

#[test]
fn bool_true() {
    assert_eq!(roundtrip(&true), OwnedValue::Bool(true));
}

#[test]
fn bool_false() {
    assert_eq!(roundtrip(&false), OwnedValue::Bool(false));
}

#[test]
fn u8_zero() {
    assert_eq!(roundtrip(&0u8), OwnedValue::U8(0));
}

#[test]
fn u8_max() {
    assert_eq!(roundtrip(&255u8), OwnedValue::U8(255));
}

#[test]
fn i8_positive() {
    assert_eq!(roundtrip(&127i8), OwnedValue::I8(127));
}

#[test]
fn i8_negative() {
    assert_eq!(roundtrip(&(-128i8)), OwnedValue::I8(-128));
}

#[test]
fn i8_zero() {
    assert_eq!(roundtrip(&0i8), OwnedValue::I8(0));
}

// ===================== Varint Unsigned =====================

#[test]
fn u16_small() {
    assert_eq!(roundtrip(&42u16), OwnedValue::U16(42));
}

#[test]
fn u16_max() {
    assert_eq!(roundtrip(&u16::MAX), OwnedValue::U16(u16::MAX));
}

#[test]
fn u32_zero() {
    assert_eq!(roundtrip(&0u32), OwnedValue::U32(0));
}

#[test]
fn u32_large() {
    assert_eq!(roundtrip(&0xDEADBEEFu32), OwnedValue::U32(0xDEADBEEF));
}

#[test]
fn u32_max() {
    assert_eq!(roundtrip(&u32::MAX), OwnedValue::U32(u32::MAX));
}

#[test]
fn u64_zero() {
    assert_eq!(roundtrip(&0u64), OwnedValue::U64(0));
}

#[test]
fn u64_max() {
    assert_eq!(roundtrip(&u64::MAX), OwnedValue::U64(u64::MAX));
}

#[test]
fn u128_zero() {
    assert_eq!(roundtrip(&0u128), OwnedValue::U128(0));
}

#[test]
fn u128_max() {
    assert_eq!(roundtrip(&u128::MAX), OwnedValue::U128(u128::MAX));
}

#[test]
fn u128_large() {
    let v = u128::MAX / 3;
    assert_eq!(roundtrip(&v), OwnedValue::U128(v));
}

// ===================== Varint Signed (zigzag) =====================

#[test]
fn i16_positive() {
    assert_eq!(roundtrip(&1000i16), OwnedValue::I16(1000));
}

#[test]
fn i16_negative() {
    assert_eq!(roundtrip(&(-1000i16)), OwnedValue::I16(-1000));
}

#[test]
fn i16_min() {
    assert_eq!(roundtrip(&i16::MIN), OwnedValue::I16(i16::MIN));
}

#[test]
fn i16_max() {
    assert_eq!(roundtrip(&i16::MAX), OwnedValue::I16(i16::MAX));
}

#[test]
fn i32_negative() {
    assert_eq!(roundtrip(&(-123456i32)), OwnedValue::I32(-123456));
}

#[test]
fn i32_min() {
    assert_eq!(roundtrip(&i32::MIN), OwnedValue::I32(i32::MIN));
}

#[test]
fn i32_max() {
    assert_eq!(roundtrip(&i32::MAX), OwnedValue::I32(i32::MAX));
}

#[test]
fn i64_min() {
    assert_eq!(roundtrip(&i64::MIN), OwnedValue::I64(i64::MIN));
}

#[test]
fn i64_max() {
    assert_eq!(roundtrip(&i64::MAX), OwnedValue::I64(i64::MAX));
}

#[test]
fn i128_min() {
    assert_eq!(roundtrip(&i128::MIN), OwnedValue::I128(i128::MIN));
}

#[test]
fn i128_max() {
    assert_eq!(roundtrip(&i128::MAX), OwnedValue::I128(i128::MAX));
}

#[test]
fn i128_zero() {
    assert_eq!(roundtrip(&0i128), OwnedValue::I128(0));
}

#[test]
fn i128_minus_one() {
    assert_eq!(roundtrip(&(-1i128)), OwnedValue::I128(-1));
}

// ===================== Floats =====================

#[test]
fn f32_zero() {
    assert_eq!(roundtrip(&0.0f32), OwnedValue::F32(0.0));
}

#[test]
fn f32_pi() {
    let v = core::f32::consts::PI;
    assert_eq!(roundtrip(&v), OwnedValue::F32(v));
}

#[test]
fn f32_negative() {
    assert_eq!(roundtrip(&(-42.5f32)), OwnedValue::F32(-42.5));
}

#[test]
fn f32_infinity() {
    assert_eq!(roundtrip(&f32::INFINITY), OwnedValue::F32(f32::INFINITY));
}

#[test]
fn f32_nan() {
    if let OwnedValue::F32(v) = roundtrip(&f32::NAN) {
        assert!(v.is_nan());
    } else {
        panic!("expected F32");
    }
}

#[test]
fn f64_zero() {
    assert_eq!(roundtrip(&0.0f64), OwnedValue::F64(0.0));
}

#[test]
fn f64_pi() {
    let v = core::f64::consts::PI;
    assert_eq!(roundtrip(&v), OwnedValue::F64(v));
}

#[test]
fn f64_max() {
    assert_eq!(roundtrip(&f64::MAX), OwnedValue::F64(f64::MAX));
}

#[test]
fn f64_min() {
    assert_eq!(roundtrip(&f64::MIN), OwnedValue::F64(f64::MIN));
}

// ===================== Char =====================

#[test]
fn char_ascii() {
    assert_eq!(roundtrip(&'A'), OwnedValue::Char('A'));
}

#[test]
fn char_null() {
    assert_eq!(roundtrip(&'\0'), OwnedValue::Char('\0'));
}

#[test]
fn char_emoji() {
    assert_eq!(roundtrip(&'🦀'), OwnedValue::Char('🦀'));
}

#[test]
fn char_cjk() {
    assert_eq!(roundtrip(&'漢'), OwnedValue::Char('漢'));
}

// ===================== Strings =====================

#[test]
fn str_empty() {
    assert_eq!(roundtrip(&""), OwnedValue::Str(String::new()));
}

#[test]
fn str_hello() {
    assert_eq!(
        roundtrip(&"hello"),
        OwnedValue::Str("hello".into())
    );
}

#[test]
fn str_unicode() {
    assert_eq!(
        roundtrip(&"こんにちは🌸"),
        OwnedValue::Str("こんにちは🌸".into())
    );
}

#[test]
fn str_long() {
    let s: String = "x".repeat(300);
    let bytes = encode(&s.as_str());
    let decoded = decode_all(&bytes);
    assert_eq!(decoded, OwnedValue::Str(s));
}

// ===================== Arrays (fixed-size) =====================

#[test]
fn array_empty() {
    let arr: [u8; 0] = [];
    assert_eq!(roundtrip(&arr), OwnedValue::Array(vec![]));
}

#[test]
fn array_u8() {
    let arr: [u8; 3] = [1, 2, 3];
    assert_eq!(
        roundtrip(&arr),
        OwnedValue::Array(vec![
            OwnedValue::U8(1),
            OwnedValue::U8(2),
            OwnedValue::U8(3),
        ])
    );
}

#[test]
fn array_i32() {
    let arr: [i32; 2] = [-100, 200];
    assert_eq!(
        roundtrip(&arr),
        OwnedValue::Array(vec![
            OwnedValue::I32(-100),
            OwnedValue::I32(200),
        ])
    );
}

// ===================== Slices =====================

#[test]
fn slice_empty() {
    let s: &[u16] = &[];
    assert_eq!(decode_all(&encode(s)), OwnedValue::Slice(vec![]));
}

#[test]
fn slice_u16() {
    let s: &[u16] = &[10, 20, 30];
    assert_eq!(
        decode_all(&encode(s)),
        OwnedValue::Slice(vec![
            OwnedValue::U16(10),
            OwnedValue::U16(20),
            OwnedValue::U16(30),
        ])
    );
}

#[test]
fn slice_bool() {
    let s: &[bool] = &[true, false, true];
    assert_eq!(
        decode_all(&encode(s)),
        OwnedValue::Slice(vec![
            OwnedValue::Bool(true),
            OwnedValue::Bool(false),
            OwnedValue::Bool(true),
        ])
    );
}

// ===================== Tuples (via Formatter API) =====================

#[test]
fn tuple_empty() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_tuple(99, 0, |_| {});
        f.into_inner().0
    };
    assert_eq!(
        decode_all(&bytes),
        OwnedValue::Tuple {
            type_id: 99,
            fields: vec![],
        }
    );
}

#[test]
fn tuple_with_fields() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_tuple(42, 3, |f| {
            f.write_u8(1);
            f.write_bool(true);
            f.write_str("hi");
        });
        f.into_inner().0
    };
    assert_eq!(
        decode_all(&bytes),
        OwnedValue::Tuple {
            type_id: 42,
            fields: vec![
                OwnedValue::U8(1),
                OwnedValue::Bool(true),
                OwnedValue::Str("hi".into()),
            ],
        }
    );
}

// ===================== Structs (via Formatter API) =====================

#[test]
fn struct_empty() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_struct(10, 0, |_| {});
        f.into_inner().0
    };
    assert_eq!(
        decode_all(&bytes),
        OwnedValue::Struct {
            type_id: 10,
            fields: vec![],
        }
    );
}

#[test]
fn struct_with_fields() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_struct(7, 2, |f| {
            f.write_field_id(0);
            f.write_u32(12345);
            f.write_field_id(1);
            f.write_str("hello");
        });
        f.into_inner().0
    };
    assert_eq!(
        decode_all(&bytes),
        OwnedValue::Struct {
            type_id: 7,
            fields: vec![
                (0, OwnedValue::U32(12345)),
                (1, OwnedValue::Str("hello".into())),
            ],
        }
    );
}

// ===================== Nested Containers =====================

#[test]
fn array_of_arrays() {
    let arr: [[u8; 2]; 3] = [[1, 2], [3, 4], [5, 6]];
    assert_eq!(
        roundtrip(&arr),
        OwnedValue::Array(vec![
            OwnedValue::Array(vec![OwnedValue::U8(1), OwnedValue::U8(2)]),
            OwnedValue::Array(vec![OwnedValue::U8(3), OwnedValue::U8(4)]),
            OwnedValue::Array(vec![OwnedValue::U8(5), OwnedValue::U8(6)]),
        ])
    );
}

#[test]
fn struct_containing_tuple() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_struct(100, 1, |f| {
            f.write_field_id(0);
            f.with_tuple(200, 2, |f| {
                f.write_i64(-999);
                f.write_char('Z');
            });
        });
        f.into_inner().0
    };
    assert_eq!(
        decode_all(&bytes),
        OwnedValue::Struct {
            type_id: 100,
            fields: vec![(
                0,
                OwnedValue::Tuple {
                    type_id: 200,
                    fields: vec![OwnedValue::I64(-999), OwnedValue::Char('Z')],
                }
            )],
        }
    );
}

#[test]
fn struct_with_array_field() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_struct(50, 2, |f| {
            f.write_field_id(0);
            f.write_str("name");
            f.write_field_id(1);
            f.write_array(&[10u8, 20u8, 30u8]);
        });
        f.into_inner().0
    };
    assert_eq!(
        decode_all(&bytes),
        OwnedValue::Struct {
            type_id: 50,
            fields: vec![
                (0, OwnedValue::Str("name".into())),
                (
                    1,
                    OwnedValue::Array(vec![
                        OwnedValue::U8(10),
                        OwnedValue::U8(20),
                        OwnedValue::U8(30),
                    ])
                ),
            ],
        }
    );
}

#[test]
fn triple_nested_struct() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_struct(1, 1, |f| {
            f.write_field_id(0);
            f.with_struct(2, 1, |f| {
                f.write_field_id(0);
                f.with_struct(3, 1, |f| {
                    f.write_field_id(0);
                    f.write_u64(42);
                });
            });
        });
        f.into_inner().0
    };
    assert_eq!(
        decode_all(&bytes),
        OwnedValue::Struct {
            type_id: 1,
            fields: vec![(
                0,
                OwnedValue::Struct {
                    type_id: 2,
                    fields: vec![(
                        0,
                        OwnedValue::Struct {
                            type_id: 3,
                            fields: vec![(0, OwnedValue::U64(42))],
                        }
                    )],
                }
            )],
        }
    );
}

// ===================== Streaming (byte-at-a-time) =====================

#[test]
fn streamed_u8() {
    assert_eq!(roundtrip_streamed(&42u8), OwnedValue::U8(42));
}

#[test]
fn streamed_string() {
    assert_eq!(
        roundtrip_streamed(&"hello world"),
        OwnedValue::Str("hello world".into())
    );
}

#[test]
fn streamed_i128_min() {
    assert_eq!(roundtrip_streamed(&i128::MIN), OwnedValue::I128(i128::MIN));
}

#[test]
fn streamed_struct() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_struct(7, 2, |f| {
            f.write_field_id(0);
            f.write_u32(12345);
            f.write_field_id(1);
            f.write_str("hello");
        });
        f.into_inner().0
    };
    assert_eq!(
        decode_byte_at_a_time(&bytes),
        OwnedValue::Struct {
            type_id: 7,
            fields: vec![
                (0, OwnedValue::U32(12345)),
                (1, OwnedValue::Str("hello".into())),
            ],
        }
    );
}

#[test]
fn streamed_nested_arrays() {
    let arr: [[u8; 2]; 2] = [[0xAA, 0xBB], [0xCC, 0xDD]];
    assert_eq!(
        roundtrip_streamed(&arr),
        OwnedValue::Array(vec![
            OwnedValue::Array(vec![OwnedValue::U8(0xAA), OwnedValue::U8(0xBB)]),
            OwnedValue::Array(vec![OwnedValue::U8(0xCC), OwnedValue::U8(0xDD)]),
        ])
    );
}

// ===================== Chunked feeding =====================

#[test]
fn chunked_feeding() {
    // Feed the encoded data in random-sized chunks
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_struct(5, 3, |f| {
            f.write_field_id(0);
            f.write_u64(u64::MAX);
            f.write_field_id(1);
            f.write_str("testing chunked decode");
            f.write_field_id(2);
            f.write_f64(core::f64::consts::E);
        });
        f.into_inner().0
    };

    // Feed in chunks of 3
    let mut decoder = Decoder::new();
    let mut offset = 0;
    loop {
        let end = (offset + 3).min(bytes.len());
        let chunk = &bytes[offset..end];
        let (consumed, result) = decoder.feed(chunk);
        offset += consumed;
        match result {
            FeedResult::Incomplete => {
                if offset >= bytes.len() {
                    panic!("Incomplete after all bytes consumed");
                }
            }
            FeedResult::Done(v) => {
                assert_eq!(
                    v,
                    OwnedValue::Struct {
                        type_id: 5,
                        fields: vec![
                            (0, OwnedValue::U64(u64::MAX)),
                            (1, OwnedValue::Str("testing chunked decode".into())),
                            (2, OwnedValue::F64(core::f64::consts::E)),
                        ],
                    }
                );
                return;
            }
            FeedResult::Error(e) => panic!("decode error: {:?}", e),
        }
    }
}

// ===================== Error Cases =====================

#[test]
fn invalid_tag() {
    let mut decoder = Decoder::new();
    let (_, result) = decoder.feed(&[0xFF]);
    assert_eq!(result, FeedResult::Error(DecodeError::InvalidTag(0xFF)));
}

#[test]
fn invalid_char() {
    // Encode a char tag followed by a varint for an invalid unicode scalar (0xD800 = surrogate)
    use rcard_log::formatter::tags::TAG_CHAR;
    let mut bytes = vec![TAG_CHAR];
    // LEB128 encode 0xD800 = 55296
    let mut v: u64 = 0xD800;
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            bytes.push(byte);
            break;
        }
        bytes.push(byte | 0x80);
    }
    let decoded = decode_all_result(&bytes);
    assert!(matches!(decoded, FeedResult::Error(DecodeError::InvalidChar)));
}

fn decode_all_result(bytes: &[u8]) -> FeedResult {
    let mut decoder = Decoder::new();
    let (_consumed, result) = decoder.feed(bytes);
    result
}

// ===================== Wire format verification =====================

#[test]
fn u8_wire_format() {
    let bytes = encode(&42u8);
    assert_eq!(bytes, vec![0x02, 42]); // TAG_U8, raw byte
}

#[test]
fn bool_wire_format() {
    assert_eq!(encode(&true), vec![0x01, 1]);
    assert_eq!(encode(&false), vec![0x01, 0]);
}

#[test]
fn unit_wire_format() {
    assert_eq!(encode(&()), vec![0x00]);
}

#[test]
fn i8_wire_format() {
    assert_eq!(encode(&(-1i8)), vec![0x03, 0xFF]);
}

#[test]
fn u16_wire_format_small() {
    // u16 value 5: TAG_U16 + LEB128(5) = [0x04, 0x05]
    assert_eq!(encode(&5u16), vec![0x04, 0x05]);
}

#[test]
fn u16_wire_format_128() {
    // u16 value 128: TAG_U16 + LEB128(128) = [0x04, 0x80, 0x01]
    assert_eq!(encode(&128u16), vec![0x04, 0x80, 0x01]);
}

#[test]
fn i16_wire_format_negative_one() {
    // zigzag(-1) = 1, LEB128(1) = 0x01
    assert_eq!(encode(&(-1i16)), vec![0x05, 0x01]);
}

#[test]
fn i16_wire_format_one() {
    // zigzag(1) = 2, LEB128(2) = 0x02
    assert_eq!(encode(&1i16), vec![0x05, 0x02]);
}

#[test]
fn str_wire_format() {
    let bytes = encode(&"hi");
    // TAG_STR + LEB128(2) + b"hi"
    assert_eq!(bytes, vec![0x0F, 0x02, b'h', b'i']);
}

#[test]
fn empty_str_wire_format() {
    let bytes = encode(&"");
    assert_eq!(bytes, vec![0x0F, 0x00]);
}

#[test]
fn f32_wire_format() {
    let bytes = encode(&1.0f32);
    let mut expected = vec![0x0C];
    expected.extend_from_slice(&1.0f32.to_le_bytes());
    assert_eq!(bytes, expected);
}

#[test]
fn f64_wire_format() {
    let bytes = encode(&1.0f64);
    let mut expected = vec![0x0D];
    expected.extend_from_slice(&1.0f64.to_le_bytes());
    assert_eq!(bytes, expected);
}

// ===================== Varint encoding properties =====================

#[test]
fn small_values_are_compact() {
    // u32 value 0 should be 2 bytes: tag + single varint byte
    assert_eq!(encode(&0u32).len(), 2);
    // u64 value 127 should be 2 bytes
    assert_eq!(encode(&127u64).len(), 2);
    // u64 value 128 needs 3 bytes (tag + 2 varint bytes)
    assert_eq!(encode(&128u64).len(), 3);
}

#[test]
fn zigzag_small_negatives_are_compact() {
    // i32 value -1 → zigzag 1 → 1 varint byte → 2 total
    assert_eq!(encode(&(-1i32)).len(), 2);
    // i64 value 0 → zigzag 0 → 1 varint byte → 2 total
    assert_eq!(encode(&0i64).len(), 2);
}

// ===================== Multiple sequential values =====================

#[test]
fn decode_sequential_values() {
    let mut f = Formatter::new(VecWriter(Vec::new()));
    42u8.format(&mut f);
    true.format(&mut f);
    "test".format(&mut f);
    let bytes = f.into_inner().0;

    let mut decoder = Decoder::new();
    let mut offset = 0;

    // First value: u8(42)
    let (consumed, result) = decoder.feed(&bytes[offset..]);
    assert_eq!(result, FeedResult::Done(OwnedValue::U8(42)));
    offset += consumed;

    // Second value: bool(true)
    let mut decoder = Decoder::new();
    let (consumed, result) = decoder.feed(&bytes[offset..]);
    assert_eq!(result, FeedResult::Done(OwnedValue::Bool(true)));
    offset += consumed;

    // Third value: str("test")
    let mut decoder = Decoder::new();
    let (consumed, result) = decoder.feed(&bytes[offset..]);
    assert_eq!(result, FeedResult::Done(OwnedValue::Str("test".into())));
    offset += consumed;

    assert_eq!(offset, bytes.len());
}

// ===================== Edge cases =====================

#[test]
fn single_element_array() {
    let arr: [u8; 1] = [99];
    assert_eq!(
        roundtrip(&arr),
        OwnedValue::Array(vec![OwnedValue::U8(99)])
    );
}

#[test]
fn struct_with_large_field_ids() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_struct(0, 1, |f| {
            f.write_field_id(10000);
            f.write_unit();
        });
        f.into_inner().0
    };
    assert_eq!(
        decode_all(&bytes),
        OwnedValue::Struct {
            type_id: 0,
            fields: vec![(10000, OwnedValue::Unit)],
        }
    );
}

#[test]
fn struct_with_max_type_id() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_struct(u64::MAX, 1, |f| {
            f.write_field_id(u64::MAX);
            f.write_bool(false);
        });
        f.into_inner().0
    };
    assert_eq!(
        decode_all(&bytes),
        OwnedValue::Struct {
            type_id: u64::MAX,
            fields: vec![(u64::MAX, OwnedValue::Bool(false))],
        }
    );
}

#[test]
fn tuple_with_mixed_types() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_tuple(0, 7, |f| {
            f.write_u8(1);
            f.write_i8(-1);
            f.write_u16(1000);
            f.write_i32(-50000);
            f.write_f32(3.14);
            f.write_str("mixed");
            f.write_bool(true);
        });
        f.into_inner().0
    };
    let result = decode_all(&bytes);
    assert_eq!(
        result,
        OwnedValue::Tuple {
            type_id: 0,
            fields: vec![
                OwnedValue::U8(1),
                OwnedValue::I8(-1),
                OwnedValue::U16(1000),
                OwnedValue::I32(-50000),
                OwnedValue::F32(3.14),
                OwnedValue::Str("mixed".into()),
                OwnedValue::Bool(true),
            ],
        }
    );
}

// Also test that byte-at-a-time produces identical results to bulk feed
#[test]
fn streamed_vs_bulk_identical() {
    let bytes = {
        let mut f = Formatter::new(VecWriter(Vec::new()));
        f.with_struct(999, 3, |f| {
            f.write_field_id(0);
            f.write_u128(u128::MAX / 2);
            f.write_field_id(1);
            f.write_array(&[1i32, -2i32, 3i32]);
            f.write_field_id(2);
            f.with_tuple(888, 2, |f| {
                f.write_str("nested");
                f.write_f64(-0.0);
            });
        });
        f.into_inner().0
    };

    let bulk = decode_all(&bytes);
    let streamed = decode_byte_at_a_time(&bytes);
    assert_eq!(bulk, streamed);
}

// ===================== Format trait impls =====================

// Verify that the Format trait impls for &[T] work correctly
#[test]
fn format_trait_slice_ref() {
    let data: Vec<u32> = vec![100, 200, 300];
    let bytes = encode(data.as_slice());
    assert_eq!(
        decode_all(&bytes),
        OwnedValue::Slice(vec![
            OwnedValue::U32(100),
            OwnedValue::U32(200),
            OwnedValue::U32(300),
        ])
    );
}

// ===================== Derive macro tests =====================

#[derive(rcard_log::Format)]
struct SimpleStruct {
    x: u32,
    y: i16,
}

#[derive(rcard_log::Format)]
struct TupleStruct(u8, bool);

#[derive(rcard_log::Format)]
struct UnitStruct;

#[derive(rcard_log::Format)]
struct SingleField {
    value: u64,
}

#[derive(rcard_log::Format)]
#[format(display = "{x} x {y}")]
struct HintedStruct {
    #[format(display = "hex")]
    x: u32,
    y: i16,
}

#[derive(rcard_log::Format)]
enum SimpleEnum {
    UnitA,
    UnitB,
    Named { a: u8, b: u16 },
    Tuple(u32, bool),
}

#[test]
fn derive_simple_struct() {
    let val = SimpleStruct { x: 42, y: -7 };
    let bytes = encode(&val);
    let decoded = decode_all(&bytes);

    // Should be a Struct with 2 fields
    match &decoded {
        OwnedValue::Struct { fields, .. } => {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].1, OwnedValue::U32(42));
            assert_eq!(fields[1].1, OwnedValue::I16(-7));
        }
        other => panic!("expected Struct, got {:?}", other),
    }
}

#[test]
fn derive_tuple_struct() {
    let val = TupleStruct(255, true);
    let bytes = encode(&val);
    let decoded = decode_all(&bytes);

    match &decoded {
        OwnedValue::Tuple { fields, .. } => {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0], OwnedValue::U8(255));
            assert_eq!(fields[1], OwnedValue::Bool(true));
        }
        other => panic!("expected Tuple, got {:?}", other),
    }
}

#[test]
fn derive_unit_struct() {
    let val = UnitStruct;
    let bytes = encode(&val);
    let decoded = decode_all(&bytes);

    match &decoded {
        OwnedValue::Struct { fields, .. } => {
            assert_eq!(fields.len(), 0);
        }
        other => panic!("expected Struct with 0 fields, got {:?}", other),
    }
}

#[test]
fn derive_single_field_struct() {
    let val = SingleField { value: 999 };
    let bytes = encode(&val);
    let decoded = decode_all(&bytes);

    match &decoded {
        OwnedValue::Struct { fields, .. } => {
            assert_eq!(fields.len(), 1);
            assert_eq!(fields[0].1, OwnedValue::U64(999));
        }
        other => panic!("expected Struct, got {:?}", other),
    }
}

#[test]
fn derive_hinted_struct_encodes_same() {
    // Hints don't affect wire format, just metadata
    let val = HintedStruct { x: 0xFF, y: 10 };
    let bytes = encode(&val);
    let decoded = decode_all(&bytes);

    match &decoded {
        OwnedValue::Struct { fields, .. } => {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].1, OwnedValue::U32(0xFF));
            assert_eq!(fields[1].1, OwnedValue::I16(10));
        }
        other => panic!("expected Struct, got {:?}", other),
    }
}

#[test]
fn derive_enum_unit_variant() {
    let val = SimpleEnum::UnitA;
    let bytes = encode(&val);
    let decoded = decode_all(&bytes);

    match &decoded {
        OwnedValue::Struct { fields, .. } => {
            assert_eq!(fields.len(), 0);
        }
        other => panic!("expected Struct(0 fields) for unit variant, got {:?}", other),
    }
}

#[test]
fn derive_enum_named_variant() {
    let val = SimpleEnum::Named { a: 42, b: 1000 };
    let bytes = encode(&val);
    let decoded = decode_all(&bytes);

    match &decoded {
        OwnedValue::Struct { fields, .. } => {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].1, OwnedValue::U8(42));
            assert_eq!(fields[1].1, OwnedValue::U16(1000));
        }
        other => panic!("expected Struct for named variant, got {:?}", other),
    }
}

#[test]
fn derive_enum_tuple_variant() {
    let val = SimpleEnum::Tuple(12345, false);
    let bytes = encode(&val);
    let decoded = decode_all(&bytes);

    match &decoded {
        OwnedValue::Tuple { fields, .. } => {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0], OwnedValue::U32(12345));
            assert_eq!(fields[1], OwnedValue::Bool(false));
        }
        other => panic!("expected Tuple for tuple variant, got {:?}", other),
    }
}

#[test]
fn derive_struct_streamed_roundtrip() {
    let val = SimpleStruct { x: u32::MAX, y: i16::MIN };
    let bytes = encode(&val);
    let decoded = decode_byte_at_a_time(&bytes);

    match &decoded {
        OwnedValue::Struct { fields, .. } => {
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0].1, OwnedValue::U32(u32::MAX));
            assert_eq!(fields[1].1, OwnedValue::I16(i16::MIN));
        }
        other => panic!("expected Struct, got {:?}", other),
    }
}

// ===================== SliceWriter tests =====================

#[test]
fn slice_writer_basic() {
    let mut buf = [0u8; 64];
    let len = {
        let mut w = SliceWriter::new(&mut buf);
        {
            let mut f = Formatter::new(&mut w);
            f.write_u8(42);
            f.write_bool(true);
        }
        w.pos()
    };
    let decoded_a = decode_all(&buf[..2]); // u8 is 2 bytes (tag + value)
    assert_eq!(decoded_a, OwnedValue::U8(42));
    assert!(len > 0);
}

#[test]
fn slice_writer_truncates_silently() {
    let mut buf = [0u8; 2]; // tiny buffer
    let mut w = SliceWriter::new(&mut buf);
    w.write(&[1, 2, 3, 4, 5]); // more than fits
    assert_eq!(w.pos(), 2);
    assert_eq!(w.written(), &[1, 2]);
}

// ===================== Log macro smoke test =====================

#[test]
fn log_macros_compile() {
    // These just verify the macros expand and don't panic.
    // They write to NoopWriter, so no output to check.
    let x = 42u32;
    rcard_log::info!("hello");
    rcard_log::info!("value: {}", x);
    rcard_log::warn!("warning: {} and {}", x, true);
    rcard_log::debug!("debug msg");
    rcard_log::error!("error: {}", "oh no");
    rcard_log::trace!("trace");
}
