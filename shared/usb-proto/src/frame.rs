use crate::error::HeaderError;
use crate::header::FrameHeader;
use crate::ipc_reply::IpcReplyView;
use crate::ipc_request::IpcRequestView;
use crate::messages::Message;
use crate::simple::SimpleFrameView;

/// Discriminator byte identifying the frame type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    IpcRequest = 0x01,
    IpcReply = 0x02,
    Simple = 0x03,
}

impl FrameType {
    pub fn from_u8(v: u8) -> Result<Self, HeaderError> {
        match v {
            0x01 => Ok(Self::IpcRequest),
            0x02 => Ok(Self::IpcReply),
            0x03 => Ok(Self::Simple),
            other => Err(HeaderError::BadFrameType(other)),
        }
    }
}

/// A decoded frame: header + borrowed payload slice.
#[derive(Clone, Copy, Debug)]
pub struct RawFrame<'a> {
    pub header: FrameHeader,
    pub payload: &'a [u8],
}

impl<'a> RawFrame<'a> {
    /// Parse this frame as an IPC request.
    pub fn as_ipc_request(&self) -> Option<IpcRequestView<'a>> {
        if self.header.frame_type != FrameType::IpcRequest {
            return None;
        }
        IpcRequestView::from_bytes(self.payload)
    }

    /// Parse this frame as an IPC response (reply or tunnel error).
    ///
    /// Returns `Ok(IpcReplyView)` if the frame is an IPC reply, or
    /// `Err(SimpleFrameView)` if it's a simple frame (e.g. tunnel error).
    /// Returns `None` if the frame type doesn't match either.
    pub fn as_ipc_response(&self) -> Option<IpcResponse<'a>> {
        match self.header.frame_type {
            FrameType::IpcReply => {
                let view = IpcReplyView::from_bytes(self.payload)?;
                Some(IpcResponse {
                    inner: Ok(view),
                    seq: self.header.seq,
                })
            }
            FrameType::Simple => {
                let view = SimpleFrameView::from_bytes(self.payload)?;
                Some(IpcResponse {
                    inner: Err(view),
                    seq: self.header.seq,
                })
            }
            _ => None,
        }
    }

    /// Parse this frame as a simple frame.
    pub fn as_simple(&self) -> Option<SimpleFrameView<'a>> {
        if self.header.frame_type != FrameType::Simple {
            return None;
        }
        SimpleFrameView::from_bytes(self.payload)
    }

    /// Convenience: parse as a simple frame and try to decode a typed message.
    pub fn parse_simple<M: Message>(&self) -> Option<M> {
        self.as_simple()?.parse::<M>()
    }
}

/// Response to an IPC request on the host-driven channel.
///
/// Wraps either an [`IpcReplyView`] (the IPC call executed) or a
/// [`SimpleFrameView`] (tunnel-level error, e.g. task dead).
///
/// Use [`.parse::<T>()`](Self::parse) to decode the IPC return value,
/// or [`.parse_simple::<M>()`](Self::parse_simple) to decode a tunnel
/// error message:
///
/// ```ignore
/// let response = frame.as_ipc_response()?;
///
/// if let Some(brightness) = response.parse::<Brightness>() {
///     return Ok(brightness);
/// }
///
/// if let Some(err) = response.parse_simple::<TunnelError>() {
///     return Err(err.code.into());
/// }
/// ```
pub struct IpcResponse<'a> {
    inner: Result<IpcReplyView<'a>, SimpleFrameView<'a>>,
    /// Sequence number from the frame header.
    pub seq: u16,
}

impl<'a> IpcResponse<'a> {
    /// Try to parse the IPC reply's return value as a zerocopy type.
    ///
    /// Returns `None` if this was a simple frame response, or if the
    /// return value bytes don't parse as `T`.
    pub fn parse<T: zerocopy::TryFromBytes + zerocopy::KnownLayout + zerocopy::Immutable>(
        &self,
    ) -> Option<T> {
        self.inner.as_ref().ok()?.parse::<T>()
    }

    /// Try to parse as a simple protocol message (e.g. tunnel error).
    ///
    /// Returns `None` if this was an IPC reply, or if the opcode doesn't
    /// match `M::OPCODE`.
    pub fn parse_simple<M: Message>(&self) -> Option<M> {
        self.inner.as_ref().err()?.parse::<M>()
    }

    /// The raw IPC reply view, if this was an IPC reply.
    pub fn as_reply(&self) -> Option<&IpcReplyView<'a>> {
        self.inner.as_ref().ok()
    }

    /// The raw simple frame view, if this was a simple frame.
    pub fn as_simple(&self) -> Option<&SimpleFrameView<'a>> {
        self.inner.as_ref().err()
    }

    /// Whether this response is an IPC reply (vs. a simple frame).
    pub fn is_reply(&self) -> bool {
        self.inner.is_ok()
    }
}
