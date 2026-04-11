//! Wire-format constants shared between `sysmodule_log` (device side)
//! and the host-side decoder.
//!
//! Every COBS chunk on the host-channel USART starts with a one-byte
//! type discriminator so the receiver can tell log fragments apart from
//! tunneled-IPC traffic. The rest of the COBS chunk is type-specific.
//!
//! The top bit (0x80) is reserved for direction-flipping future types if
//! needed; 0x00–0x7F is shared namespace for both directions.

/// Log fragment (device → host).
///
/// Payload layout after the type byte:
/// ```text
///   [type = 0x01][stream_id: u64 LE][len: u8][data: len bytes]
/// ```
/// Multiple fragments with the same `stream_id` are reassembled into one
/// log entry by the host.
pub const TYPE_LOG_FRAGMENT: u8 = 0x01;

/// IPC reply or tunnel error (device → host).
///
/// Payload after the type byte is a full `rcard_usb_proto` frame
/// (header + body) — the same wire format `usb_protocol_host` writes
/// onto EP1 IN. The host can feed it to its existing `FrameReader`
/// after stripping the type byte.
pub const TYPE_IPC_REPLY: u8 = 0x02;

/// IPC request (host → device).
///
/// Payload after the type byte is a full `rcard_usb_proto` frame.
/// `sysmodule_log`'s RX handler strips the type byte, stages the frame
/// bytes in its `PendingRequest` slot, and wakes `host_proxy` via the
/// `host_request` notification group.
pub const TYPE_IPC_REQUEST: u8 = 0x03;

/// Control request (host → device).
///
/// Payload after the type byte is a full `rcard_usb_proto` simple frame.
/// Used for lightweight, non-tunneled host→device control messages that
/// `sysmodule_log` can handle directly (e.g. `MoshiMoshi` → `Hello`
/// handshake pings) without going through `host_proxy`.
pub const TYPE_CONTROL_REQUEST: u8 = 0x04;
