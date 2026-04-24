//! Runtime IPC interpreter for the host.
//!
//! Given a set of postcard-schema type descriptions (loaded from a `.tfw`
//! archive's `ipc-metadata.json`), this crate can:
//!
//! 1. Accept user-supplied [`IpcValue`] arguments for an IPC method call.
//! 2. Validate them against the method's schema.
//! 3. Encode them into postcard wire bytes + lease data, matching what
//!    the firmware's `#[derive(Serialize)]` + `postcard::to_slice` produces.
//! 4. Decode a postcard-encoded reply back into an `IpcValue`.
//!
//! The host never links against firmware api crates at compile time — it
//! discovers the IPC surface at runtime from the tfw metadata.

pub mod value;
pub mod encode;
pub mod decode;
pub mod registry;

pub use value::IpcValue;
pub use registry::{
    extract_handle, CallError, EncodedCall, HandleExtractError, MethodKind, MethodSchema,
    Registry, ReplyDecodeError,
};
