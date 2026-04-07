/// Trait for typed USB protocol messages.
///
/// Each message type declares its opcode and how to parse/serialize
/// its payload. This enables `frame.try_parse::<Ping>()` style usage.
pub trait Message: Sized {
    /// The opcode byte that identifies this message on the wire.
    const OPCODE: u8;

    /// Try to parse the payload bytes into this message type.
    /// Returns `None` if the payload is malformed.
    fn from_payload(payload: &[u8]) -> Option<Self>;

    /// Serialize this message's payload into `buf`.
    /// Returns the number of bytes written, or `None` if the buffer is too small.
    fn to_payload(&self, buf: &mut [u8]) -> Option<usize>;
}
