#![no_std]

use rcard_log::Format;

macro_rules! cap_enum {
    ($(#[$meta:meta])* $vis:vis enum $name:ident { $($(#[$vmeta:meta])* $variant:ident = $val:expr),* $(,)? }) => {
        #[derive(
            Clone, Copy, Debug,
            zerocopy::TryFromBytes, zerocopy::IntoBytes,
            zerocopy::KnownLayout, zerocopy::Immutable,
            Format,
            serde::Serialize, serde::Deserialize,
            postcard_schema::Schema,
        )]
        #[repr(u8)]
        $(#[$meta])*
        $vis enum $name { $($(#[$vmeta])* $variant = $val),* }
    };
}

macro_rules! cap_struct {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($(#[$fmeta:meta])* $fvis:vis $field:ident : $ty:ty),* $(,)? }) => {
        #[derive(
            Clone, Copy, Debug,
            zerocopy::TryFromBytes, zerocopy::IntoBytes,
            zerocopy::KnownLayout, zerocopy::Immutable,
            Format,
            serde::Serialize, serde::Deserialize,
            postcard_schema::Schema,
        )]
        #[repr(C, packed)]
        $(#[$meta])*
        $vis struct $name { $($(#[$fmeta])* $fvis $field : $ty),* }
    };
}

cap_enum! {
    pub enum Cap1208Error {
        InvalidDevice = 0,
        AlreadyOpen = 1,
        I2cNack = 2,
        I2cTimeout = 3,
        UnexpectedProductId = 4,
    }
}

cap_enum! {
    #[derive(PartialEq, Eq)]
    pub enum Device { A = 0, B = 1 }
}

// ── Signal ─────────────────────────────────────────────────────────

cap_enum! {
    /// Analog gain applied before digitization (register 0x1F bits [6:4]).
    /// Higher values detect smaller capacitance changes but saturate
    /// the signed 8-bit delta register sooner.
    pub enum AnalogGain {
        X128 = 0,
        X64 = 1,
        X32 = 2,
        X16 = 3,
        X8 = 4,
        X4 = 5,
        X2 = 6,
        X1 = 7,
    }
}

cap_enum! {
    /// Right-shift applied to the raw count before storing in the 8-bit
    /// base register (register 0x1F bits [3:0]).  Lower values preserve
    /// more base-count resolution, reducing discontinuities during
    /// recalibration at the cost of a smaller representable range.
    pub enum DigitalShift {
        X1 = 0,
        X2 = 1,
        X4 = 2,
        X8 = 3,
        X16 = 4,
        X32 = 5,
        X64 = 6,
        X128 = 7,
        X256 = 8,
    }
}

cap_struct! {
    pub struct SignalConfig {
        pub analog_gain: AnalogGain,
        pub digital_shift: DigitalShift,
    }
}

impl SignalConfig {
    pub const fn new(analog_gain: AnalogGain, digital_shift: DigitalShift) -> Self {
        Self { analog_gain, digital_shift }
    }
}

// ── Sampling ───────────────────────────────────────────────────────

cap_enum! {
    /// Samples averaged per channel per scan cycle (register 0x24 bits [6:4]).
    pub enum Averaging {
        Avg1 = 0,
        Avg2 = 1,
        Avg4 = 2,
        Avg8 = 3,
        Avg16 = 4,
        Avg32 = 5,
        Avg64 = 6,
        Avg128 = 7,
    }
}

cap_enum! {
    /// Per-sample ADC integration time (register 0x24 bits [3:2]).
    pub enum Duration {
        Us320 = 0,
        Us640 = 1,
        Ms1_28 = 2,
        Ms2_56 = 3,
    }
}

cap_enum! {
    /// Minimum period between consecutive full scan cycles
    /// (register 0x24 bits [1:0]).
    pub enum CycleTime {
        Ms35 = 0,
        Ms70 = 1,
        Ms105 = 2,
        Ms140 = 3,
    }
}

cap_struct! {
    pub struct SamplingConfig {
        pub averaging: Averaging,
        pub duration: Duration,
        pub cycle_time: CycleTime,
    }
}

impl SamplingConfig {
    pub const fn fastest(averaging: Averaging, duration: Duration) -> Self {
        Self { averaging, duration, cycle_time: CycleTime::Ms35 }
    }
}

// ── Recalibration ──────────────────────────────────────────────────

cap_enum! {
    /// Maximum touch hold time before the chip recalibrates that channel
    /// (register 0x20 bit 3 + register 0x22 bits [7:4]).
    /// `Disabled` means a touch can be held indefinitely without
    /// triggering recalibration.
    pub enum TouchRecalDuration {
        Disabled = 0,
        Ms560 = 1,
        Ms840 = 2,
        Ms1120 = 3,
        Ms1400 = 4,
        Ms1680 = 5,
        Ms2240 = 6,
        Ms2800 = 7,
        Ms3360 = 8,
        Ms3920 = 9,
        Ms4480 = 10,
        Ms5600 = 11,
        Ms6720 = 12,
        Ms7840 = 13,
        Ms8960 = 14,
        Ms10080 = 15,
        Ms11200 = 16,
    }
}

cap_enum! {
    /// Consecutive negative-delta readings required to trigger a digital
    /// recalibration (register 0x2F bits [4:3]).
    /// `Disabled` means negative deltas never trigger recalibration.
    pub enum BelowBaseline {
        Count8 = 0,
        Count16 = 1,
        Count32 = 2,
        Disabled = 3,
    }
}

cap_enum! {
    /// Background recalibration averaging depth and update period
    /// (register 0x2F bits [2:0]).  The update period is in multiples
    /// of the sensing cycle time.
    pub enum RecalRate {
        /// 16 samples, update every 16 cycles
        Samples16 = 0,
        /// 32 samples, update every 32 cycles
        Samples32 = 1,
        /// 64 samples, update every 64 cycles
        Samples64 = 2,
        /// 128 samples, update every 128 cycles
        Samples128 = 3,
        /// 256 samples, update every 256 cycles
        Samples256 = 4,
        /// 256 samples, update every 1024 cycles
        Samples256Slow = 5,
        /// 256 samples, update every 2048 cycles
        Samples256Slower = 6,
        /// 256 samples, update every 4096 cycles
        Samples256Slowest = 7,
    }
}

cap_struct! {
    pub struct RecalibrationConfig {
        pub touch_duration: TouchRecalDuration,
        pub below_baseline: BelowBaseline,
        pub rate: RecalRate,
    }
}

impl RecalibrationConfig {
    pub const fn every(rate: RecalRate) -> Self {
        Self {
            touch_duration: TouchRecalDuration::Disabled,
            below_baseline: BelowBaseline::Disabled,
            rate,
        }
    }

    pub const fn with_touch_duration(mut self, duration: TouchRecalDuration) -> Self {
        self.touch_duration = duration;
        self
    }

    pub const fn with_below_baseline(mut self, count: BelowBaseline) -> Self {
        self.below_baseline = count;
        self
    }
}

// ── Top-level config ───────────────────────────────────────────────

cap_struct! {
    pub struct Cap1208Config {
        pub enabled_channels: u8,
        pub signal: SignalConfig,
        pub sampling: SamplingConfig,
        pub recalibration: RecalibrationConfig,
    }
}

// ── Output types ───────────────────────────────────────────────────

cap_struct! {
    pub struct DeviceId {
        pub product_id: u8,
        pub manufacturer_id: u8,
        pub revision: u8,
    }
}

// ── IPC trait ──────────────────────────────────────────────────────

#[ipc::resource(arena_size = 2, kind = 0x0C)]
pub trait Cap1208 {
    #[constructor]
    fn open(device: Device, config: Cap1208Config) -> Result<Self, Cap1208Error>;

    #[message]
    fn read(&mut self) -> Result<[i32; 8], Cap1208Error>;

    /// Force recalibration of selected channels (bitmask, bit 0 = CS1).
    #[message]
    fn calibrate(&self, channels: u8) -> Result<(), Cap1208Error>;

    #[message]
    fn read_id(&self) -> Result<DeviceId, Cap1208Error>;
}
