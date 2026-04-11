/// Opcode for log entry frames on the fob-driven channel.
///
/// The payload is raw `consume_since` wire data — one or more entries
/// concatenated:
///
/// ```text
/// [log_id: u64 LE][data_len: u8][fragment_idx: u8][data: data_len bytes]
/// ```
///
/// No `Message` trait impl — the payload is variable-length raw bytes,
/// not a fixed struct.
pub const OP_LOG_ENTRY: u8 = 0x20;
