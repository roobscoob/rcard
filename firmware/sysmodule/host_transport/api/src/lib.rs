#![no_std]

//! Shared transport interface used by `host_proxy` to reach wire-framed
//! IPC tunnels. Every transport task (USB EP1, USART2, future others)
//! implements this trait with a different IPC `kind`, and `host_proxy`
//! binds each one under a distinct alias via `bind_host_transport!`.
//!
//! The pattern is "stage one, dispatch one":
//!   - The transport drains its wire and stashes the next complete
//!     `IpcRequest` frame (as produced by `rcard_usb_proto::FrameReader`)
//!     in a single-slot buffer.
//!   - It pushes the `host_request` reactor notification.
//!   - `host_proxy` wakes, calls `fetch_pending_request` on the transport
//!     whose task index matches the notification sender, runs the
//!     tunneled dispatch, and hands the encoded `IpcReply` frame back via
//!     `deliver_reply`.
//!   - `deliver_reply` writes the reply to the wire and clears the slot.

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum HostTransportError {
    /// No request is currently staged. `host_proxy` saw a `host_request`
    /// notification but the transport has nothing to hand over — usually
    /// a spurious or duplicated notification.
    NoPendingRequest = 0,
    /// The caller's buffer is smaller than the staged request / reply.
    LeaseTooSmall = 1,
    /// Writing the reply to the wire failed.
    WireWriteFailed = 2,
}

#[ipc::resource(arena_size = 0, kind = 0x34)]
pub trait HostTransport {
    /// Copy the staged pending request into the caller's write-lease.
    /// Returns the number of bytes written. Errors if no request is
    /// pending or the lease is smaller than the staged frame.
    #[message]
    fn fetch_pending_request(#[lease] buf: &mut [u8]) -> Result<u32, HostTransportError>;

    /// Accept a reply frame from the caller's read-lease and emit it on
    /// the transport's wire. Clears the pending-request slot on success.
    #[message]
    fn deliver_reply(#[lease] buf: &[u8]) -> Result<(), HostTransportError>;
}