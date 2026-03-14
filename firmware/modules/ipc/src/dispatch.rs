//! Type-safe server-side dispatch runtime.
//!
//! The codegen emits calls into this module rather than raw kernel syscalls.
//! The type system enforces protocol invariants:
//! - Exactly one reply per message (`PendingReply` is move-only)
//! - Lease access permissions checked at construction
//! - Lease lifetime bound to message lifetime
//! - Deserialization errors map to `ReplyFaultReason`

use core::marker::PhantomData;

use crate::kern;

// ── Access marker types ──────────────────────────────────────────────

/// Marker: read-only lease access.
pub enum Read {}
/// Marker: write-only lease access.
pub enum Write {}

/// Sealed trait for lease access modes.
pub trait Access: sealed::Sealed {
    fn check(atts: kern::LeaseAttributes) -> bool;
}

impl Access for Read {
    #[inline]
    fn check(atts: kern::LeaseAttributes) -> bool {
        atts.contains(kern::LeaseAttributes::READ)
    }
}

impl Access for Write {
    #[inline]
    fn check(atts: kern::LeaseAttributes) -> bool {
        atts.contains(kern::LeaseAttributes::WRITE)
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Read {}
    impl Sealed for super::Write {}
}

// ── LeaseBorrow ──────────────────────────────────────────────────────

/// A lease accessor tied to the lifetime of an incoming message.
///
/// Can only be constructed by [`MessageData::lease`] — codegen cannot
/// fabricate one. The lifetime `'msg` ensures the lease cannot outlive
/// the message (and thus the reply).
pub struct LeaseBorrow<'msg, A: Access> {
    sender: kern::TaskId,
    index: usize,
    length: usize,
    _access: PhantomData<A>,
    _life: PhantomData<&'msg ()>,
}

impl<A: Access> LeaseBorrow<'_, A> {
    /// Number of bytes in this lease.
    #[inline]
    pub fn len(&self) -> usize {
        self.length
    }

    /// Whether the lease is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

impl LeaseBorrow<'_, Read> {
    /// Read a single byte at `index`.
    #[inline]
    pub fn read(&self, index: usize) -> Option<u8> {
        let mut buf = [0u8; 1];
        let n = kern::sys_borrow_read(self.sender, self.index, index, &mut buf)?;
        if n >= 1 { Some(buf[0]) } else { None }
    }

    /// Read a contiguous range starting at `offset` into `dest`.
    #[inline]
    pub fn read_range(&self, offset: usize, dest: &mut [u8]) -> Option<usize> {
        kern::sys_borrow_read(self.sender, self.index, offset, dest)
    }
}

impl LeaseBorrow<'_, Write> {
    /// Read a single byte at `index` (write leases may also be readable).
    #[inline]
    pub fn read(&self, index: usize) -> Option<u8> {
        let mut buf = [0u8; 1];
        let n = kern::sys_borrow_read(self.sender, self.index, index, &mut buf)?;
        if n >= 1 { Some(buf[0]) } else { None }
    }

    /// Read a contiguous range (write leases may also be readable).
    #[inline]
    pub fn read_range(&self, offset: usize, dest: &mut [u8]) -> Option<usize> {
        kern::sys_borrow_read(self.sender, self.index, offset, dest)
    }

    /// Write a single byte at `index`.
    #[inline]
    pub fn write(&self, index: usize, value: u8) -> Option<usize> {
        kern::sys_borrow_write(self.sender, self.index, index, &[value])
    }

    /// Write a contiguous range starting at `offset`.
    #[inline]
    pub fn write_range(&self, offset: usize, src: &[u8]) -> Option<usize> {
        kern::sys_borrow_write(self.sender, self.index, offset, src)
    }
}

// ── PendingReply ─────────────────────────────────────────────────────

/// A reply token that must be consumed exactly once.
///
/// - Consuming via `reply_ok` / `reply_serialize` / `reply_error` sends
///   the reply to the kernel.
/// - Dropping without consuming sends a `ReplyFaultReason::BadMessageContents`
///   fault, ensuring the client is never left hanging.
///
/// Not `Clone`, not `Copy`. The codegen receives this from
/// `MessageData::take_reply()` and must pass it to a reply method.
pub struct PendingReply {
    sender: kern::TaskId,
    replied: bool,
}

impl PendingReply {
    /// Reply with success and a serialized payload.
    #[inline]
    pub fn reply_ok(mut self, data: &[u8]) {
        kern::sys_reply(self.sender, kern::ResponseCode::SUCCESS, data);
        self.replied = true;
    }

    /// Reply with a non-success response code.
    #[inline]
    pub fn reply_error(mut self, code: kern::ResponseCode, data: &[u8]) {
        kern::sys_reply(self.sender, code, data);
        self.replied = true;
    }

    /// Reply with a zerocopy-serializable value and SUCCESS.
    #[inline]
    pub fn reply_val<T: zerocopy::IntoBytes + zerocopy::Immutable>(mut self, value: &T) {
        let bytes = zerocopy::IntoBytes::as_bytes(value);
        kern::sys_reply(self.sender, kern::ResponseCode::SUCCESS, bytes);
        self.replied = true;
    }

    /// Get the sender's TaskId.
    #[inline]
    pub fn sender(&self) -> kern::TaskId {
        self.sender
    }
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        if !self.replied {
            kern::sys_reply_fault(self.sender, kern::ReplyFaultReason::BadMessageContents);
        }
    }
}

// ── MessageData ──────────────────────────────────────────────────────

/// The payload of an incoming message, after the reply token has been
/// split off. Provides typed access to arguments and leases.
///
/// Only constructed by the server dispatch loop — codegen cannot fabricate.
pub struct MessageData<'buf> {
    sender: kern::TaskId,
    operation: u16,
    data: &'buf [u8],
    lease_count: usize,
}

impl MessageData<'_> {
    /// Deserialize arguments from the message payload via zerocopy.
    ///
    /// Reads a `T` from the front of the payload, returning it and the
    /// remaining bytes (for sequential multi-arg decoding in codegen).
    pub fn read_arg<T: zerocopy::TryFromBytes + zerocopy::KnownLayout + zerocopy::Immutable>(
        &self,
    ) -> Result<T, kern::ReplyFaultReason> {
        crate::wire::read::<T>(self.data)
            .map(|(val, _)| val)
            .ok_or(kern::ReplyFaultReason::BadMessageContents)
    }

    /// Extract a lease with the given access mode.
    ///
    /// Returns `Err(BadLeases)` if the index is out of bounds or the
    /// lease doesn't have the required permissions.
    pub fn lease<A: Access>(
        &self,
        index: usize,
    ) -> Result<LeaseBorrow<'_, A>, kern::ReplyFaultReason> {
        if index >= self.lease_count {
            return Err(kern::ReplyFaultReason::BadLeases);
        }
        let info =
            kern::sys_borrow_info(self.sender, index).ok_or(kern::ReplyFaultReason::BadLeases)?;
        if !A::check(info.atts) {
            return Err(kern::ReplyFaultReason::BadLeases);
        }
        Ok(LeaseBorrow {
            sender: self.sender,
            index,
            length: info.len,
            _access: PhantomData,
            _life: PhantomData,
        })
    }

    /// The sender's TaskId.
    #[inline]
    pub fn sender(&self) -> kern::TaskId {
        self.sender
    }

    /// The sender's task index.
    #[inline]
    pub fn sender_index(&self) -> u16 {
        self.sender.task_index()
    }

    /// The raw operation code.
    #[inline]
    pub fn operation(&self) -> u16 {
        self.operation
    }

    /// Number of leases the sender provided.
    #[inline]
    pub fn lease_count(&self) -> usize {
        self.lease_count
    }

    /// Build an `ipc::Meta` for passing to handler methods.
    #[inline]
    pub fn meta(&self) -> crate::Meta {
        crate::Meta {
            sender: self.sender,
            lease_count: self.lease_count as u8,
        }
    }

    /// Raw message bytes (for manual deserialization).
    #[inline]
    pub fn raw_data(&self) -> &[u8] {
        self.data
    }
}

// ── Incoming message constructor (crate-private) ─────────────────────

/// Create a `MessageData` + `PendingReply` pair from a raw kernel message.
///
/// This is `pub(crate)` — only the server dispatch loop calls it.
/// Codegen and user code cannot construct these directly.
pub(crate) fn split_message<'buf>(
    msg: &kern::Message<'buf>,
) -> Result<(MessageData<'buf>, PendingReply), kern::ReplyFaultReason> {
    let data = match &msg.data {
        Ok(d) => *d,
        Err(_) => return Err(kern::ReplyFaultReason::BadMessageSize),
    };

    let md = MessageData {
        sender: msg.sender,
        operation: msg.operation,
        data,
        lease_count: msg.lease_count,
    };

    let reply = PendingReply {
        sender: msg.sender,
        replied: false,
    };

    Ok((md, reply))
}
