#![no_std]

use rcard_log::Format;

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
#[repr(u8)]
pub enum Ldo {
    Vdd33Ldo3 = 0,
}

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
#[repr(u8)]
pub enum SysconError {
    InvalidLdo = 0,
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

#[ipc::resource(arena_size = 0, kind = 0x0B)]
pub trait Syscon {
    #[message]
    fn enable_ldo(ldo: Ldo) -> Result<(), SysconError>;

    #[message]
    fn disable_ldo(ldo: Ldo) -> Result<(), SysconError>;

    /// Read `HPSYS_CFG.IDR` and return the four ID bytes. Always
    /// succeeds — interpretation is up to the caller via
    /// [`ChipId::rev`].
    #[message]
    fn chip_id() -> ChipId;
}
