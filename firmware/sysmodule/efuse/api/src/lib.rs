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
pub enum EfuseError {
    /// `bank_id` was not in the range 0..=3.
    InvalidBank = 0,
    /// The eFuse controller did not complete the read in time.
    Timeout = 1,
}

/// Subset of Bank1 calibration fields needed by `sysmodule_lcpu::rf_cal`.
///
/// Bit positions match sifli-rs `sifli-hal/src/efuse/bank1.rs` (commit
/// aa4c19c). We only expose the fields RF cal actually reads — the
/// remainder of Bank1 (BUCK / LDO trims, ADC voltage cal, Vol2 variants,
/// etc.) is not consumed by anything in this firmware yet.
///
/// Field meanings:
/// - `edr_cal_done` (Bank1 bit 125): when set, the factory has
///   programmed EDR PA cal values into `dac_lsb_cnt` and `pa_bm`. When
///   clear, RF cal's `apply_edr_power_cal` returns `None` and EDR uses
///   the ROM defaults.
/// - `pa_bm` (Bank1 bits 126..127): PA bias margin selector. Valid
///   values per sifli-rs `apply_edr_power_cal`: 0, 1, 3.
/// - `dac_lsb_cnt` (Bank1 bits 128..129): DAC LSB count adjustment
///   applied to TBB_REG during TXDC cal.
/// - `tmxcap_flag` (Bank1 bit 130): per-channel TMXCAP cavity-tuning
///   table is present in `tmxcap_ch00` / `tmxcap_ch78`.
/// - `tmxcap_ch78` (Bank1 bits 131..134): TMXCAP for the channel-78 end
///   of the band.
/// - `tmxcap_ch00` (Bank1 bits 135..138): TMXCAP for the channel-0 end.
///
/// Adapted from `sifli-rs/sifli-hal/src/efuse/bank1.rs::Bank1PrimaryLow`
/// + `Bank1PrimaryHigh`. License: Apache-2.0 (upstream).
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
pub struct Bank1Calibration {
    pub edr_cal_done: u8, // bool packed as u8 for zerocopy-friendly layout
    pub pa_bm: u8,
    pub dac_lsb_cnt: u8,
    pub tmxcap_flag: u8,
    pub tmxcap_ch78: u8,
    pub tmxcap_ch00: u8,
    /// Padding so the struct is naturally aligned and a multiple of its
    /// largest member size.
    _reserved: [u8; 2],
}

impl Bank1Calibration {
    /// Decode a Bank1 readback (eight 32-bit words, little-endian) into
    /// the named fields. Bit-extraction matches sifli-rs's
    /// `Bank1Calibration::decode` (`sifli-hal/src/efuse/bank1.rs:36`).
    pub const fn decode(words: &[u32; 8]) -> Self {
        // Bank1PrimaryLow occupies bits 0..127 (words 0..3).
        let low: u128 = (words[0] as u128)
            | ((words[1] as u128) << 32)
            | ((words[2] as u128) << 64)
            | ((words[3] as u128) << 96);
        // Bank1PrimaryHigh occupies bits 128..159 (low 32 bits of words[4]).
        let high: u32 = words[4];

        // PrimaryLow field offsets — see bank1.rs:60..145.
        let edr_cal_done = ((low >> 125) & 0x1) as u8;
        let pa_bm = ((low >> 126) & 0x3) as u8;

        // PrimaryHigh field offsets — see bank1.rs:147..169.
        let dac_lsb_cnt = (high & 0x3) as u8;
        let tmxcap_flag = ((high >> 2) & 0x1) as u8;
        let tmxcap_ch78 = ((high >> 3) & 0xF) as u8;
        let tmxcap_ch00 = ((high >> 7) & 0xF) as u8;

        Self {
            edr_cal_done,
            pa_bm,
            dac_lsb_cnt,
            tmxcap_flag,
            tmxcap_ch78,
            tmxcap_ch00,
            _reserved: [0; 2],
        }
    }
}

#[ipc::resource(arena_size = 0, kind = 0x06)]
pub trait Efuse {
    /// Read one full 256-bit eFuse bank (banks 0..=3) as a 32-byte array.
    ///
    /// The controller's read sequence is run synchronously and the eight
    /// 32-bit bank words are serialized little-endian into the return
    /// buffer. Readback masking (bank0 fuse bits 244..=253) is applied
    /// by the hardware — a masked bank reads as all zeros.
    #[message]
    fn read(bank_id: u8) -> Result<[u8; 32], EfuseError>;

    /// Read Bank1 and decode the calibration fields RF cal needs.
    ///
    /// Equivalent to `read(1)` followed by `Bank1Calibration::decode`,
    /// done server-side so callers don't have to depend on the decoder.
    /// Returns `EfuseError::Timeout` if the controller doesn't complete.
    #[message]
    fn read_calibration() -> Result<Bank1Calibration, EfuseError>;
}
