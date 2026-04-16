//! Decode postcard wire bytes back into an `IpcValue` using a schema.
//!
//! Placeholder — will be implemented when reply decoding is needed.
//! For now, methods with no return type (like `Log::log`) don't need
//! this.

use crate::value::IpcValue;
use postcard_schema::schema::owned::OwnedNamedType;

/// Decode a postcard-encoded reply into an `IpcValue`.
pub fn decode(_schema: &OwnedNamedType, _bytes: &[u8]) -> Result<IpcValue, DecodeError> {
    // TODO: implement schema-driven postcard deserialization.
    // For void-returning methods this is never called.
    Ok(IpcValue::Unit)
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
