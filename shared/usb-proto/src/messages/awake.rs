use super::Message;

/// Opcode for the [`Awake`] message.
pub const OP_AWAKE: u8 = 0x02;

/// Length of a single [`Awake`] field (chip UID or firmware id) in bytes.
pub const AWAKE_FIELD_SIZE: usize = 16;

/// Total payload size of the [`Awake`] message: chip UID + firmware id.
pub const AWAKE_PAYLOAD_SIZE: usize = AWAKE_FIELD_SIZE * 2;

/// Sent as a [`SimpleFrame`](crate::simple::SimpleFrameView) by
/// `sysmodule_log` exactly once on startup, after USART2 is open and
/// the IPC server is about to run.
///
/// The host treats this as an authoritative "the device is up and
/// talking" signal on the control channel — independent of any log
/// traffic which may or may not be flowing yet.
///
/// Payload layout (32 bytes total):
/// ```text
///   [0..16]   chip UID   — eFuse bank 0, identifies the physical chip
///   [16..32]  build id   — UUIDv4 from the firmware build pipeline,
///                          identifies which firmware image is running
/// ```
///
/// The chip UID lets the host associate all channels on the same
/// device across reboots. The build id lets the host look up matching
/// `.tfw` metadata (log species, type hashes, source locations) so the
/// log viewer can render structured logs with full fidelity.
pub struct Awake {
    pub uid: [u8; AWAKE_FIELD_SIZE],
    pub firmware_id: [u8; AWAKE_FIELD_SIZE],
}

impl Awake {
    /// Construct an Awake carrying a chip UID and firmware build id.
    pub const fn new(
        uid: [u8; AWAKE_FIELD_SIZE],
        firmware_id: [u8; AWAKE_FIELD_SIZE],
    ) -> Self {
        Self { uid, firmware_id }
    }
}

impl Message for Awake {
    const OPCODE: u8 = OP_AWAKE;

    fn from_payload(payload: &[u8]) -> Option<Self> {
        if payload.len() < AWAKE_PAYLOAD_SIZE {
            return None;
        }
        let mut uid = [0u8; AWAKE_FIELD_SIZE];
        let mut firmware_id = [0u8; AWAKE_FIELD_SIZE];
        uid.copy_from_slice(&payload[..AWAKE_FIELD_SIZE]);
        firmware_id.copy_from_slice(&payload[AWAKE_FIELD_SIZE..AWAKE_PAYLOAD_SIZE]);
        Some(Self { uid, firmware_id })
    }

    fn to_payload(&self, buf: &mut [u8]) -> Option<usize> {
        if buf.len() < AWAKE_PAYLOAD_SIZE {
            return None;
        }
        buf[..AWAKE_FIELD_SIZE].copy_from_slice(&self.uid);
        buf[AWAKE_FIELD_SIZE..AWAKE_PAYLOAD_SIZE].copy_from_slice(&self.firmware_id);
        Some(AWAKE_PAYLOAD_SIZE)
    }
}
