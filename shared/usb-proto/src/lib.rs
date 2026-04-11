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
