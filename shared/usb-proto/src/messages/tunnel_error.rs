use super::Message;

pub const OP_TUNNEL_ERROR: u8 = 0x01;

/// Tunnel-level error codes sent as a [`SimpleFrame`](crate::simple::SimpleFrameView)
/// in response to an IPC request that could not be dispatched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TunnelErrorCode {
    /// The target task is dead or has restarted (generation mismatch).
    TaskDead = 0x01,
    /// The request's leases exceed the tunnel's 8K buffer pool.
    LeasePoolFull = 0x02,
    /// The request frame is malformed.
    BadRequest = 0x03,
    /// This firmware build does not include the host-IPC dispatcher
    /// (`sysmodule_host_proxy`). The transport accepted the frame but
    /// has nowhere to forward it. The host should treat this as a
    /// permanent capability error, not retry.
    NoHostForwarding = 0x04,
    /// The transport already has an in-flight request in its single
    /// pending slot. The synchronous protocol shouldn't allow this in
    /// normal operation, but a retrying host can see it. Safe to retry
    /// after the previous reply is delivered.
    Busy = 0x05,
    /// Unspecified internal error in the tunnel sysmodule.
    Internal = 0xFF,
}

impl TunnelErrorCode {
    pub fn from_byte(v: u8) -> Self {
        match v {
            0x01 => Self::TaskDead,
            0x02 => Self::LeasePoolFull,
            0x03 => Self::BadRequest,
            0x04 => Self::NoHostForwarding,
            0x05 => Self::Busy,
            _ => Self::Internal,
        }
    }
}

pub struct TunnelError {
    pub code: TunnelErrorCode,
}

impl Message for TunnelError {
    const OPCODE: u8 = OP_TUNNEL_ERROR;

    fn from_payload(payload: &[u8]) -> Option<Self> {
        if payload.is_empty() {
            return None;
        }
        Some(Self {
            code: TunnelErrorCode::from_byte(payload[0]),
        })
    }

    fn to_payload(&self, buf: &mut [u8]) -> Option<usize> {
        if buf.is_empty() {
            return None;
        }
        buf[0] = self.code as u8;
        Some(1)
    }
}
