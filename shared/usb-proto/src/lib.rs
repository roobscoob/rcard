#![no_std]

#[cfg(feature = "std")]
extern crate std;

pub mod error;
pub mod frame;
pub mod header;
pub mod ipc_reply;
pub mod ipc_request;
pub mod messages;
pub mod reader;
pub mod simple;
pub mod writer;

pub use error::HeaderError;
pub use frame::{FrameType, IpcResponse, RawFrame};
pub use header::{FrameHeader, HEADER_SIZE};
pub use ipc_reply::{IpcReply, IpcReplyView};
pub use ipc_request::{
    IpcRequest, IpcRequestView, LeaseDescriptor, LeaseKind, LEASE_POOL_SIZE, MAX_ARGS,
};
pub use messages::Message;
pub use reader::{FrameReader, ReaderError};
pub use simple::SimpleFrameView;
pub use writer::FrameWriter;

/// Maximum number of lease descriptors per IPC request/reply.
pub const MAX_LEASES: usize = 4;

/// Maximum decoded frame size (header + largest payload).
///
/// The largest payload is an IPC request:
///   fixed_fields(6) + args(256) + descriptors(4×2) + lease_data(8192) = 8462
/// This exceeds the IPC reply payload:
///   fixed_fields(5) + return_value(256) + writeback(8192) = 8453
///
/// Previous code used a hardcoded 8704 which accidentally baked in COBS
/// encoding overhead. Decoded frame buffers don't need that headroom.
pub const MAX_DECODED_FRAME: usize = {
    // IpcRequest payload (the larger direction)
    let request_fixed = 6; // task_id(2) + kind(1) + method(1) + lease_count(1) + args_len(1)
    let request_payload = request_fixed
        + MAX_ARGS
        + MAX_LEASES * ipc_request::LEASE_DESCRIPTOR_SIZE
        + LEASE_POOL_SIZE;
    HEADER_SIZE + request_payload
};
