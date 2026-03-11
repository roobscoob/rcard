extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::formatter::tags::*;
use crate::OwnedValue;

#[derive(Debug, PartialEq)]
pub enum DecodeError {
    InvalidTag(u8),
    VarintOverflow,
    InvalidUtf8,
    InvalidChar,
}

#[derive(Debug, PartialEq)]
pub enum FeedResult {
    Incomplete,
    Done(OwnedValue),
    EndOfStream,
    Error(DecodeError),
}

pub struct Decoder {
    state: State,
    stack: Vec<Frame>,
}

enum State {
    AwaitingTag,
    ReadingSingleByte {
        tag: u8,
    },
    ReadingVarint {
        acc: u128,
        shift: u8,
        purpose: VarintPurpose,
    },
    ReadingFixed {
        buf: [u8; 8],
        expected: u8,
        got: u8,
        is_f64: bool,
    },
    ReadingString {
        buf: Vec<u8>,
        remaining: u64,
    },
}

enum VarintPurpose {
    UnsignedValue { tag: u8 },
    SignedValue { tag: u8 },
    CharValue,
    StringLength,
    ContainerLength { tag: u8 },
    TupleTypeId,
    TupleFieldCount { type_id: u64 },
    StructTypeId,
    StructFieldCount { type_id: u64 },
    StructFieldId,
}

enum Frame {
    Array {
        remaining: u64,
        children: Vec<OwnedValue>,
    },
    Slice {
        remaining: u64,
        children: Vec<OwnedValue>,
    },
    Tuple {
        type_id: u64,
        remaining: u64,
        fields: Vec<OwnedValue>,
    },
    Struct {
        type_id: u64,
        remaining: u64,
        current_field_id: u64,
        fields: Vec<(u64, OwnedValue)>,
    },
}

impl Decoder {
    pub fn new() -> Self {
        Decoder {
            state: State::AwaitingTag,
            stack: Vec::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> (usize, FeedResult) {
        for (i, &byte) in bytes.iter().enumerate() {
            match self.feed_byte(byte) {
                FeedResult::Incomplete => {}
                result => return (i + 1, result),
            }
        }
        (bytes.len(), FeedResult::Incomplete)
    }

    fn feed_byte(&mut self, byte: u8) -> FeedResult {
        // Take ownership of state to avoid borrow issues
        let state = core::mem::replace(&mut self.state, State::AwaitingTag);

        match state {
            State::AwaitingTag => self.handle_tag(byte),

            State::ReadingSingleByte { tag } => {
                let value = match tag {
                    TAG_BOOL => OwnedValue::Bool(byte != 0),
                    TAG_U8 => OwnedValue::U8(byte),
                    TAG_I8 => OwnedValue::I8(byte as i8),
                    _ => return FeedResult::Error(DecodeError::InvalidTag(tag)),
                };
                self.complete_value(value)
            }

            State::ReadingVarint {
                acc,
                shift,
                purpose,
            } => {
                let value = (byte & 0x7F) as u128;

                if shift >= 128 {
                    return FeedResult::Error(DecodeError::VarintOverflow);
                }

                let acc = acc | (value << shift);

                if byte & 0x80 != 0 {
                    self.state = State::ReadingVarint {
                        acc,
                        shift: shift + 7,
                        purpose,
                    };
                    FeedResult::Incomplete
                } else {
                    self.handle_varint_complete(acc, purpose)
                }
            }

            State::ReadingFixed {
                mut buf,
                expected,
                got,
                is_f64,
            } => {
                buf[got as usize] = byte;
                let got = got + 1;
                if got < expected {
                    self.state = State::ReadingFixed {
                        buf,
                        expected,
                        got,
                        is_f64,
                    };
                    FeedResult::Incomplete
                } else if is_f64 {
                    self.complete_value(OwnedValue::F64(f64::from_le_bytes(buf)))
                } else {
                    let mut b4 = [0u8; 4];
                    b4.copy_from_slice(&buf[..4]);
                    self.complete_value(OwnedValue::F32(f32::from_le_bytes(b4)))
                }
            }

            State::ReadingString { mut buf, remaining } => {
                buf.push(byte);
                let remaining = remaining - 1;
                if remaining > 0 {
                    self.state = State::ReadingString { buf, remaining };
                    FeedResult::Incomplete
                } else {
                    match String::from_utf8(buf) {
                        Ok(s) => self.complete_value(OwnedValue::Str(s)),
                        Err(_) => FeedResult::Error(DecodeError::InvalidUtf8),
                    }
                }
            }
        }
    }

    fn handle_tag(&mut self, tag: u8) -> FeedResult {
        match tag {
            TAG_UNIT => self.complete_value(OwnedValue::Unit),

            TAG_BOOL | TAG_U8 | TAG_I8 => {
                self.state = State::ReadingSingleByte { tag };
                FeedResult::Incomplete
            }

            TAG_U16 | TAG_U32 | TAG_U64 | TAG_U128 => {
                self.state = State::ReadingVarint {
                    acc: 0,
                    shift: 0,
                    purpose: VarintPurpose::UnsignedValue { tag },
                };
                FeedResult::Incomplete
            }

            TAG_I16 | TAG_I32 | TAG_I64 | TAG_I128 => {
                self.state = State::ReadingVarint {
                    acc: 0,
                    shift: 0,
                    purpose: VarintPurpose::SignedValue { tag },
                };
                FeedResult::Incomplete
            }

            TAG_F32 => {
                self.state = State::ReadingFixed {
                    buf: [0; 8],
                    expected: 4,
                    got: 0,
                    is_f64: false,
                };
                FeedResult::Incomplete
            }

            TAG_F64 => {
                self.state = State::ReadingFixed {
                    buf: [0; 8],
                    expected: 8,
                    got: 0,
                    is_f64: true,
                };
                FeedResult::Incomplete
            }

            TAG_CHAR => {
                self.state = State::ReadingVarint {
                    acc: 0,
                    shift: 0,
                    purpose: VarintPurpose::CharValue,
                };
                FeedResult::Incomplete
            }

            TAG_STR => {
                self.state = State::ReadingVarint {
                    acc: 0,
                    shift: 0,
                    purpose: VarintPurpose::StringLength,
                };
                FeedResult::Incomplete
            }

            TAG_ARRAY | TAG_SLICE => {
                self.state = State::ReadingVarint {
                    acc: 0,
                    shift: 0,
                    purpose: VarintPurpose::ContainerLength { tag },
                };
                FeedResult::Incomplete
            }

            TAG_TUPLE => {
                self.state = State::ReadingVarint {
                    acc: 0,
                    shift: 0,
                    purpose: VarintPurpose::TupleTypeId,
                };
                FeedResult::Incomplete
            }

            TAG_STRUCT => {
                self.state = State::ReadingVarint {
                    acc: 0,
                    shift: 0,
                    purpose: VarintPurpose::StructTypeId,
                };
                FeedResult::Incomplete
            }

            TAG_END_OF_STREAM => FeedResult::EndOfStream,

            _ => FeedResult::Error(DecodeError::InvalidTag(tag)),
        }
    }

    fn handle_varint_complete(&mut self, raw: u128, purpose: VarintPurpose) -> FeedResult {
        match purpose {
            VarintPurpose::UnsignedValue { tag } => {
                let value = match tag {
                    TAG_U16 => OwnedValue::U16(raw as u16),
                    TAG_U32 => OwnedValue::U32(raw as u32),
                    TAG_U64 => OwnedValue::U64(raw as u64),
                    TAG_U128 => OwnedValue::U128(raw),
                    _ => unreachable!(),
                };
                self.complete_value(value)
            }

            VarintPurpose::SignedValue { tag } => {
                let value = match tag {
                    TAG_I16 => {
                        let zz = raw as u64;
                        OwnedValue::I16(((zz >> 1) as i64 ^ -((zz & 1) as i64)) as i16)
                    }
                    TAG_I32 => {
                        let zz = raw as u64;
                        OwnedValue::I32(((zz >> 1) as i64 ^ -((zz & 1) as i64)) as i32)
                    }
                    TAG_I64 => {
                        let zz = raw as u64;
                        OwnedValue::I64((zz >> 1) as i64 ^ -((zz & 1) as i64))
                    }
                    TAG_I128 => OwnedValue::I128((raw >> 1) as i128 ^ -((raw & 1) as i128)),
                    _ => unreachable!(),
                };
                self.complete_value(value)
            }

            VarintPurpose::CharValue => match char::from_u32(raw as u32) {
                Some(c) => self.complete_value(OwnedValue::Char(c)),
                None => FeedResult::Error(DecodeError::InvalidChar),
            },

            VarintPurpose::StringLength => {
                if raw == 0 {
                    self.complete_value(OwnedValue::Str(String::new()))
                } else {
                    self.state = State::ReadingString {
                        buf: Vec::with_capacity(raw as usize),
                        remaining: raw as u64,
                    };
                    FeedResult::Incomplete
                }
            }

            VarintPurpose::ContainerLength { tag } => {
                let count = raw as u64;
                if count == 0 {
                    let value = if tag == TAG_ARRAY {
                        OwnedValue::Array(Vec::new())
                    } else {
                        OwnedValue::Slice(Vec::new())
                    };
                    self.complete_value(value)
                } else {
                    let frame = if tag == TAG_ARRAY {
                        Frame::Array {
                            remaining: count,
                            children: Vec::with_capacity(count as usize),
                        }
                    } else {
                        Frame::Slice {
                            remaining: count,
                            children: Vec::with_capacity(count as usize),
                        }
                    };
                    self.stack.push(frame);
                    self.state = State::AwaitingTag;
                    FeedResult::Incomplete
                }
            }

            VarintPurpose::TupleTypeId => {
                self.state = State::ReadingVarint {
                    acc: 0,
                    shift: 0,
                    purpose: VarintPurpose::TupleFieldCount {
                        type_id: raw as u64,
                    },
                };
                FeedResult::Incomplete
            }

            VarintPurpose::TupleFieldCount { type_id } => {
                let count = raw as u64;
                if count == 0 {
                    self.complete_value(OwnedValue::Tuple {
                        type_id,
                        fields: Vec::new(),
                    })
                } else {
                    self.stack.push(Frame::Tuple {
                        type_id,
                        remaining: count,
                        fields: Vec::with_capacity(count as usize),
                    });
                    self.state = State::AwaitingTag;
                    FeedResult::Incomplete
                }
            }

            VarintPurpose::StructTypeId => {
                self.state = State::ReadingVarint {
                    acc: 0,
                    shift: 0,
                    purpose: VarintPurpose::StructFieldCount {
                        type_id: raw as u64,
                    },
                };
                FeedResult::Incomplete
            }

            VarintPurpose::StructFieldCount { type_id } => {
                let count = raw as u64;
                if count == 0 {
                    self.complete_value(OwnedValue::Struct {
                        type_id,
                        fields: Vec::new(),
                    })
                } else {
                    self.stack.push(Frame::Struct {
                        type_id,
                        remaining: count,
                        current_field_id: 0,
                        fields: Vec::with_capacity(count as usize),
                    });
                    // Read first field_id
                    self.state = State::ReadingVarint {
                        acc: 0,
                        shift: 0,
                        purpose: VarintPurpose::StructFieldId,
                    };
                    FeedResult::Incomplete
                }
            }

            VarintPurpose::StructFieldId => {
                if let Some(Frame::Struct {
                    current_field_id, ..
                }) = self.stack.last_mut()
                {
                    *current_field_id = raw as u64;
                }
                self.state = State::AwaitingTag;
                FeedResult::Incomplete
            }
        }
    }

    fn complete_value(&mut self, value: OwnedValue) -> FeedResult {
        let mut value = value;
        loop {
            let frame = match self.stack.last_mut() {
                None => return FeedResult::Done(value),
                Some(f) => f,
            };

            match frame {
                Frame::Array {
                    remaining,
                    children,
                }
                | Frame::Slice {
                    remaining,
                    children,
                } => {
                    children.push(value);
                    *remaining -= 1;
                    if *remaining == 0 {
                        let frame = self.stack.pop().unwrap();
                        value = match frame {
                            Frame::Array { children, .. } => OwnedValue::Array(children),
                            Frame::Slice { children, .. } => OwnedValue::Slice(children),
                            _ => unreachable!(),
                        };
                        continue;
                    } else {
                        self.state = State::AwaitingTag;
                        return FeedResult::Incomplete;
                    }
                }

                Frame::Tuple {
                    remaining, fields, ..
                } => {
                    fields.push(value);
                    *remaining -= 1;
                    if *remaining == 0 {
                        let frame = self.stack.pop().unwrap();
                        value = match frame {
                            Frame::Tuple {
                                type_id, fields, ..
                            } => OwnedValue::Tuple { type_id, fields },
                            _ => unreachable!(),
                        };
                        continue;
                    } else {
                        self.state = State::AwaitingTag;
                        return FeedResult::Incomplete;
                    }
                }

                Frame::Struct {
                    remaining,
                    current_field_id,
                    fields,
                    ..
                } => {
                    fields.push((*current_field_id, value));
                    *remaining -= 1;
                    if *remaining == 0 {
                        let frame = self.stack.pop().unwrap();
                        value = match frame {
                            Frame::Struct {
                                type_id, fields, ..
                            } => OwnedValue::Struct { type_id, fields },
                            _ => unreachable!(),
                        };
                        continue;
                    } else {
                        // Read next field_id
                        self.state = State::ReadingVarint {
                            acc: 0,
                            shift: 0,
                            purpose: VarintPurpose::StructFieldId,
                        };
                        return FeedResult::Incomplete;
                    }
                }
            }
        }
    }
}
