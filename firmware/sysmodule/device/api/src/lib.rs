#![no_std]

use rcard_log::Format;

/// Length (in bytes) of the ASCII hex rendering of a 16-byte chip UID.
pub const UID_HEX_LEN: usize = 32;

/// Render a 16-byte chip UID as a 32-character uppercase hex string.
///
/// Writes into `buf` and returns a `&str` borrowed from it — the caller
/// owns the buffer, so the resulting `&str` is valid for as long as
/// `buf` is. Intended for wiring the UID into places that want a
/// `&str`, like the USB `DeviceIdentity::serial` descriptor.
pub fn uid_to_hex<'a>(uid: &[u8; 16], buf: &'a mut [u8; UID_HEX_LEN]) -> &'a str {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for (i, byte) in uid.iter().enumerate() {
        buf[i * 2] = HEX[(byte >> 4) as usize];
        buf[i * 2 + 1] = HEX[(byte & 0x0F) as usize];
    }
    // SAFETY: every byte we just wrote is in the ASCII hex alphabet.
    unsafe { core::str::from_utf8_unchecked(buf) }
}

/// Parsed `HPSYS_CFG.IDR` register. Mirrors the four bytes the SiFli SDK
/// uses for chip identification.
#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(C)]
pub struct ChipId {
    /// Hardware revision ID (`IDR[7:0]`). Distinguishes silicon families;
    /// see [`ChipId::rev`] for the classification.
    pub revid: u8,
    /// Package ID (`IDR[15:8]`).
    pub pid: u8,
    /// Company ID (`IDR[23:16]`).
    pub cid: u8,
    /// Series ID (`IDR[31:24]`).
    pub sid: u8,
}

/// Classified chip revision. Computed from `ChipId::revid` by
/// [`ChipId::rev`]; clients match on this to pick rev-specific code
/// paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Format)]
pub enum ChipRev {
    /// `revid 0x00..=0x03` — A3 silicon or earlier. Boots from RAM and
    /// uses the 64-byte ROM-config layout.
    A3OrEarlier,
    /// `revid 0x07` (A4) or `0x0F` (B4) — Letter-series silicon. Boots
    /// from internal ROM and uses the 204-byte ROM-config layout.
    Letter,
}

impl ChipId {
    /// Classify [`Self::revid`] into a [`ChipRev`]. Returns `None` if
    /// the byte doesn't match any known family — callers should refuse
    /// to bring up rev-sensitive subsystems rather than guess.
    pub const fn rev(&self) -> Option<ChipRev> {
        match self.revid {
            0x00..=0x03 => Some(ChipRev::A3OrEarlier),
            0x07 | 0x0F => Some(ChipRev::Letter),
            _ => None,
        }
    }
}

#[ipc::resource(arena_size = 0, kind = 0x07)]
pub trait Device {
    /// Return the device's 16-byte chip UID, read once at boot from
    /// eFuse bank 0 via `sysmodule_efuse`.
    #[message]
    fn get_uid() -> [u8; 16];

    /// Return the 16-byte session ID, generated once at boot from the
    /// hardware TRNG via `sysmodule_rand`. Stable for the lifetime of
    /// a single boot — lets consumers distinguish USB re-enumeration
    /// from a real reboot. Returns all zeros if the TRNG was unavailable.
    #[message]
    fn get_session_id() -> [u8; 16];

    /// Reset the MCU back to bootrom. Never returns.
    #[message]
    fn reset();

    /// Read `HPSYS_CFG.IDR` and return the four ID bytes. Always
    /// succeeds — interpretation is up to the caller via
    /// [`ChipId::rev`].
    #[message]
    fn chip_id() -> ChipId;
}