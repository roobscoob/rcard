use std::time::Instant;

use rcard_log::{LogLevel, OwnedValue};

use crate::adapter::AdapterId;

/// A log event with its source adapter.
#[derive(Clone, Debug)]
pub struct Log {
    pub adapter: AdapterId,
    pub contents: LogContents,
    /// Host wall-clock at which the *first byte* of this log was
    /// observed on the adapter's wire. Used by the host to order logs
    /// from multiple adapters into a single coherent stream — arrival
    /// time at the main-thread event handler is *not* sufficient,
    /// since different adapters have different decode latencies and
    /// may be gated behind discovery probes.
    pub received_at: Instant,
}

/// The content of a log event.
#[derive(Clone, Debug)]
pub enum LogContents {
    /// Decoded structured log entry (from a binary stream like USART2).
    Structured(LogEntry),
    /// Plain text line (from a text stream like USART1).
    Text(String),
    /// Named auxiliary text stream (e.g. "renode").
    Auxiliary { name: String, text: String },
    /// Renode emulator log with a parsed level and message.
    Renode { level: LogLevel, message: String },
}

/// A non-log event decoded off a control channel (e.g. USART2 IPC replies).
///
/// These flow on the same wire as structured logs but aren't logs — they
/// represent protocol-level messages (tunnel errors, IPC responses).
#[derive(Clone, Debug)]
pub enum ControlEvent {
    /// `sysmodule_log` announced itself on startup, carrying the chip
    /// UID and the firmware image's build id.
    Awake {
        seq: u16,
        uid: [u8; rcard_usb_proto::messages::AWAKE_FIELD_SIZE],
        firmware_id: [u8; rcard_usb_proto::messages::AWAKE_FIELD_SIZE],
    },
    /// A tunnel-level error frame from the device.
    TunnelError {
        code: rcard_usb_proto::messages::TunnelErrorCode,
        seq: u16,
    },
    /// A simple frame with an opcode we don't yet decode.
    UnknownSimple {
        seq: u16,
        opcode: u8,
        payload: Vec<u8>,
    },
    /// An IPC reply frame — decoding of the reply body is deferred.
    IpcReply {
        seq: u16,
        payload: Vec<u8>,
    },
    /// A malformed frame on the control channel.
    FrameError(String),
}

/// A decoded structured log entry from a binary stream.
#[derive(Clone, Debug)]
pub struct LogEntry {
    pub level: LogLevel,
    pub timestamp: u64,
    /// Task index on the device.
    pub source: u16,
    /// Unique monotonic ID for this log entry.
    pub log_id: u64,
    /// Species hash — key into the tfw metadata for format string + source location.
    pub log_species: u64,
    /// Decoded argument values from the binary payload.
    pub values: Vec<OwnedValue>,
    /// The stream ended without a `TAG_END_OF_STREAM` terminator —
    /// `values` is whatever was decodable before the host gave up
    /// waiting (stream-timeout eviction).
    pub truncated: bool,
}
