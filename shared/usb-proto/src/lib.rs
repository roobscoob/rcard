#![no_std]
#![cfg_attr(feature = "std", allow(unused_imports))]

#[cfg(feature = "std")]
extern crate std;

mod frame;
mod opcode;

pub use frame::{DecodeError, Frame, FrameHeader, FrameReader, FrameWriter, HEADER_SIZE, MAGIC};
pub use opcode::Opcode;
