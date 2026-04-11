use super::Message;

/// Opcode for the [`MoshiMoshi`] message.
pub const OP_MOSHI_MOSHI: u8 = 0x03;

/// Host → device liveness ping.
///
/// Sent as a [`SimpleFrame`](crate::simple::SimpleFrameView) wrapped in
/// a `TYPE_CONTROL_REQUEST` chunk on USART2. `sysmodule_log` responds
/// immediately with an [`Awake`](super::Awake) simple frame carrying
/// the cached chip UID and firmware build id — the same payload it
/// sends once at boot — so the host can re-discover device identity at
/// any time without waiting for a power cycle.
pub struct MoshiMoshi;

impl Message for MoshiMoshi {
    const OPCODE: u8 = OP_MOSHI_MOSHI;

    fn from_payload(_payload: &[u8]) -> Option<Self> {
        Some(Self)
    }

    fn to_payload(&self, _buf: &mut [u8]) -> Option<usize> {
        Some(0)
    }
}
