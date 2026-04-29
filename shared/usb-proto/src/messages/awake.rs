use super::Message;

/// Opcode for the [`Awake`] message.
pub const OP_AWAKE: u8 = 0x02;

/// UUID for the rcard identity BOS platform capability descriptor.
/// Mixed-endian encoding per the USB BOS / MSOS 2.0 convention.
///
/// The host matches on this UUID when parsing BOS descriptors at USB
/// attach time. The capability payload carries the same (uid, build_id,
/// session_id) triple as the USART2 Awake message.
pub const RCARD_IDENTITY_UUID: [u8; 16] = [
    0x27, 0xe5, 0x33, 0xd9, 0x38, 0xec, 0x8e, 0x45,
    0xb9, 0x4a, 0xda, 0xc5, 0xef, 0xaf, 0x56, 0xa2,
];

/// Size of the rcard identity BOS platform capability payload:
/// bReserved(1) + UUID(16) + uid(16) + build_id(16) + session_id(16).
pub const RCARD_IDENTITY_CAP_SIZE: usize = 1 + 16 + AWAKE_PAYLOAD_SIZE;

/// Length of a single [`Awake`] field (chip UID, firmware id, or session id) in bytes.
pub const AWAKE_FIELD_SIZE: usize = 16;

/// Minimum payload accepted by [`Awake::from_payload`] — the legacy
/// two-field format (uid + firmware_id) without a session id.
const AWAKE_LEGACY_SIZE: usize = AWAKE_FIELD_SIZE * 2;

/// Total payload size of the [`Awake`] message: chip UID + firmware id + session id.
pub const AWAKE_PAYLOAD_SIZE: usize = AWAKE_FIELD_SIZE * 3;

/// Sent as a [`SimpleFrame`](crate::simple::SimpleFrameView) by
/// `sysmodule_log` exactly once on startup, after USART2 is open and
/// the IPC server is about to run.
///
/// The host treats this as an authoritative "the device is up and
/// talking" signal on the control channel — independent of any log
/// traffic which may or may not be flowing yet.
///
/// Payload layout (48 bytes total):
/// ```text
///   [0..16]   chip UID    — eFuse bank 0, identifies the physical chip
///   [16..32]  build id    — UUIDv4 from the firmware build pipeline,
///                           identifies which firmware image is running
///   [32..48]  session id  — random 128 bits generated once at boot via
///                           hardware TRNG, lets the host distinguish a
///                           USB re-enumeration from a real reboot
/// ```
///
/// The chip UID lets the host associate all channels on the same
/// device across reboots. The build id lets the host look up matching
/// `.tfw` metadata (log species, type hashes, source locations) so the
/// log viewer can render structured logs with full fidelity. The
/// session id is stable for the lifetime of a single boot — if the
/// host sees the same session id in a second Awake, the device did not
/// reboot (e.g. USB re-enumerated) and in-flight operations should not
/// be torn down.
pub struct Awake {
    pub uid: [u8; AWAKE_FIELD_SIZE],
    pub firmware_id: [u8; AWAKE_FIELD_SIZE],
    pub session_id: [u8; AWAKE_FIELD_SIZE],
}

impl Awake {
    pub const fn new(
        uid: [u8; AWAKE_FIELD_SIZE],
        firmware_id: [u8; AWAKE_FIELD_SIZE],
        session_id: [u8; AWAKE_FIELD_SIZE],
    ) -> Self {
        Self {
            uid,
            firmware_id,
            session_id,
        }
    }
}

impl Message for Awake {
    const OPCODE: u8 = OP_AWAKE;

    fn from_payload(payload: &[u8]) -> Option<Self> {
        if payload.len() < AWAKE_LEGACY_SIZE {
            return None;
        }
        let mut uid = [0u8; AWAKE_FIELD_SIZE];
        let mut firmware_id = [0u8; AWAKE_FIELD_SIZE];
        let mut session_id = [0u8; AWAKE_FIELD_SIZE];
        uid.copy_from_slice(&payload[..AWAKE_FIELD_SIZE]);
        firmware_id.copy_from_slice(&payload[AWAKE_FIELD_SIZE..AWAKE_LEGACY_SIZE]);
        if payload.len() >= AWAKE_PAYLOAD_SIZE {
            session_id.copy_from_slice(&payload[AWAKE_LEGACY_SIZE..AWAKE_PAYLOAD_SIZE]);
        }
        Some(Self {
            uid,
            firmware_id,
            session_id,
        })
    }

    fn to_payload(&self, buf: &mut [u8]) -> Option<usize> {
        if buf.len() < AWAKE_PAYLOAD_SIZE {
            return None;
        }
        buf[..AWAKE_FIELD_SIZE].copy_from_slice(&self.uid);
        buf[AWAKE_FIELD_SIZE..AWAKE_LEGACY_SIZE].copy_from_slice(&self.firmware_id);
        buf[AWAKE_LEGACY_SIZE..AWAKE_PAYLOAD_SIZE].copy_from_slice(&self.session_id);
        Some(AWAKE_PAYLOAD_SIZE)
    }
}
