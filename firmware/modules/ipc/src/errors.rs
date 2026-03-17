//! Precise client-side error types for IPC methods.
//!
//! These are NOT serializable — they exist only on the client side.
//! The macro picks the right type per method based on
//! `(method_kind, has_move_params, has_clone_params)`.

use crate::arena::CloneError;
use rcard_log::Format;

/// A handle was lost (evicted, freed, wrong owner, stale, or server died).
#[derive(Debug, Format)]
pub struct HandleLostError;

// ── Constructor family (4) ─────────────────────────────

/// Constructor failed: server died or arena full.
#[derive(Debug, Format)]
pub enum ConstructorError {
    ServerDied,
    ArenaFull,
}

/// Constructor with `#[handle(move)]` params.
#[derive(Debug)]
pub enum ConstructorTransferError {
    ServerDied,
    ArenaFull,
    TransferLost(&'static str),
}

/// Constructor with `#[handle(clone)]` params.
#[derive(Debug)]
pub enum ConstructorCloneError {
    ServerDied,
    ArenaFull,
    CloneFailed(&'static str, CloneError),
}

/// Constructor with both `#[handle(move)]` and `#[handle(clone)]` params.
#[derive(Debug)]
pub enum ConstructorTransferCloneError {
    ServerDied,
    ArenaFull,
    TransferLost(&'static str),
    CloneFailed(&'static str, CloneError),
}

// ── Message family (4) — destructors use these too ─────

/// Message with `#[handle(move)]` params.
#[derive(Debug)]
pub enum MessageTransferError {
    HandleLost,
    TransferLost(&'static str),
}

/// Message with `#[handle(clone)]` params.
#[derive(Debug)]
pub enum MessageCloneError {
    HandleLost,
    CloneFailed(&'static str, CloneError),
}

/// Message with both `#[handle(move)]` and `#[handle(clone)]` params.
#[derive(Debug)]
pub enum MessageTransferCloneError {
    HandleLost,
    TransferLost(&'static str),
    CloneFailed(&'static str, CloneError),
}

// ── Static message family (4) ──────────────────────────

/// Static message (no handle params).
#[derive(Debug)]
pub enum StaticMessageError {
    ServerDied,
}

/// Static message with `#[handle(move)]` params.
#[derive(Debug)]
pub enum StaticMessageTransferError {
    ServerDied,
    TransferLost(&'static str),
}

/// Static message with `#[handle(clone)]` params.
#[derive(Debug)]
pub enum StaticMessageCloneError {
    ServerDied,
    CloneFailed(&'static str, CloneError),
}

/// Static message with both `#[handle(move)]` and `#[handle(clone)]` params.
#[derive(Debug)]
pub enum StaticMessageTransferCloneError {
    ServerDied,
    TransferLost(&'static str),
    CloneFailed(&'static str, CloneError),
}

// ── Explicit wire-error mapping ──────────────────────────
//
// No `From<Error>` — codegen calls `Type::from_wire(e)` so the
// conversion is always visible at the call site.

impl HandleLostError {
    pub fn from_wire(_: crate::Error) -> Self {
        HandleLostError
    }
}

impl ConstructorError {
    pub fn from_wire(e: crate::Error) -> Self {
        match e {
            crate::Error::ArenaFull => Self::ArenaFull,
            _ => Self::ServerDied,
        }
    }
}

impl ConstructorTransferError {
    pub fn from_wire(e: crate::Error) -> Self {
        match e {
            crate::Error::ArenaFull => Self::ArenaFull,
            _ => Self::ServerDied,
        }
    }
}

impl ConstructorCloneError {
    pub fn from_wire(e: crate::Error) -> Self {
        match e {
            crate::Error::ArenaFull => Self::ArenaFull,
            _ => Self::ServerDied,
        }
    }
}

impl ConstructorTransferCloneError {
    pub fn from_wire(e: crate::Error) -> Self {
        match e {
            crate::Error::ArenaFull => Self::ArenaFull,
            _ => Self::ServerDied,
        }
    }
}

impl StaticMessageError {
    pub fn from_wire(_: crate::Error) -> Self {
        Self::ServerDied
    }
}

impl StaticMessageTransferError {
    pub fn from_wire(_: crate::Error) -> Self {
        Self::ServerDied
    }
}

impl StaticMessageCloneError {
    pub fn from_wire(_: crate::Error) -> Self {
        Self::ServerDied
    }
}

impl StaticMessageTransferCloneError {
    pub fn from_wire(_: crate::Error) -> Self {
        Self::ServerDied
    }
}

impl MessageTransferError {
    pub fn from_wire(_: crate::Error) -> Self {
        Self::HandleLost
    }
}

impl MessageCloneError {
    pub fn from_wire(_: crate::Error) -> Self {
        Self::HandleLost
    }
}

impl MessageTransferCloneError {
    pub fn from_wire(_: crate::Error) -> Self {
        Self::HandleLost
    }
}
