use crate::ipc_reply::IpcReply;
use crate::ipc_request::IpcRequest;
use crate::messages::Message;
use crate::simple;

/// Tracks sequence numbers and encodes frames.
///
/// The internal counter tracks **outbound request** sequence numbers.
/// Reply methods (`write_ipc_reply_to`, `write_simple_to`) accept an
/// explicit seq to echo the request they're responding to and do **not**
/// advance the counter.
pub struct FrameWriter {
    seq: u16,
}

impl FrameWriter {
    pub const fn new() -> Self {
        Self { seq: 0 }
    }

    /// Allocate the next sequence number.
    fn next_seq(&mut self) -> u16 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        s
    }

    /// The next sequence number that will be assigned.
    ///
    /// This tracks outbound requests only — reply methods use the
    /// request's seq and do not advance this counter.
    pub fn current_seq(&self) -> u16 {
        self.seq
    }

    /// Encode an IPC request frame.
    pub fn write_ipc_request(&mut self, req: &IpcRequest<'_>, buf: &mut [u8]) -> Option<usize> {
        let seq = self.next_seq();
        req.encode_into(buf, seq)
    }

    /// Encode an IPC reply frame, advancing the seq counter.
    ///
    /// Use this when initiating an unsolicited reply (rare). For replies
    /// that correlate with a request, use [`write_ipc_reply_to`](Self::write_ipc_reply_to).
    pub fn write_ipc_reply(&mut self, reply: &IpcReply<'_>, buf: &mut [u8]) -> Option<usize> {
        let seq = self.next_seq();
        reply.encode_into(buf, seq)
    }

    /// Encode an IPC reply echoing a request's sequence number.
    ///
    /// Does **not** advance the internal seq counter.
    pub fn write_ipc_reply_to(
        &self,
        reply: &IpcReply<'_>,
        request_seq: u16,
        buf: &mut [u8],
    ) -> Option<usize> {
        reply.encode_into(buf, request_seq)
    }

    /// Encode a typed simple frame message.
    pub fn write_simple<M: Message>(&mut self, msg: &M, buf: &mut [u8]) -> Option<usize> {
        let seq = self.next_seq();
        simple::encode_simple(msg, buf, seq)
    }

    /// Encode a simple frame echoing a request's sequence number.
    ///
    /// Use this when replying to an IPC request with a tunnel error.
    /// Does **not** advance the internal seq counter.
    pub fn write_simple_to<M: Message>(
        &self,
        msg: &M,
        request_seq: u16,
        buf: &mut [u8],
    ) -> Option<usize> {
        simple::encode_simple(msg, buf, request_seq)
    }

    /// Encode a simple frame from a raw opcode and payload.
    pub fn write_simple_raw(
        &mut self,
        opcode: u8,
        payload: &[u8],
        buf: &mut [u8],
    ) -> Option<usize> {
        let seq = self.next_seq();
        simple::encode_simple_raw(opcode, payload, buf, seq)
    }
}
