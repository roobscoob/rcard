#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use generated::slots::SLOTS;
use rcard_log::{debug, error, info, trace, OptionExt};
use sifli_pac::mpi::Mpi as MpiPeri;
use sysmodule_mpi_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);

static MPI_IN_USE: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

static mut MPI_SCRATCH: [u8; 256] = [0u8; 256];

fn mpi_instance(index: u8) -> Option<MpiPeri> {
    match index {
        1 => Some(sifli_pac::MPI1),
        2 => Some(sifli_pac::MPI2),
        _ => None,
    }
}

// Standard SPI NOR flash commands.
//
// Address-bearing commands come in two variants: a 3-byte-address form
// (works for the first 16 MB of any chip) and a 4-byte-address form
// (required to reach beyond 16 MB on chips that support it). The driver
// picks the right opcode at each call site based on `config.addr_size`.
// Using the wrong variant is silently broken — the chip either ignores
// the command or writes/erases at a mis-parsed address.
const CMD_WRITE_ENABLE: u8 = 0x06;
const CMD_READ_STATUS_1: u8 = 0x05;
const CMD_READ_JEDEC_ID: u8 = 0x9F;
const CMD_SFDP: u8 = 0x5A;
const CMD_RESET_ENABLE: u8 = 0x66; // RST_EN — arms the chip for reset
const CMD_RESET: u8 = 0x99; // RST — performs the soft reset
const CMD_RELEASE_DPD: u8 = 0xAB; // RDPD — wakes chip from Deep Power-Down
const CMD_ENTER_4BYTE_MODE: u8 = 0xB7; // EN4B — ADS bit goes 0 → 1
const CMD_EXIT_4BYTE_MODE: u8 = 0xE9; // EX4B — ADS bit goes 1 → 0
const CMD_CHIP_ERASE: u8 = 0xC7; // CE — no address, unchanged

// 3-byte address variants
const CMD_READ_DATA_3B: u8 = 0x03;
#[allow(dead_code)]
const CMD_FAST_READ_3B: u8 = 0x0B;
const CMD_PAGE_PROGRAM_3B: u8 = 0x02;
const CMD_SECTOR_ERASE_4K_3B: u8 = 0x20;
const CMD_BLOCK_ERASE_32K_3B: u8 = 0x52;
const CMD_BLOCK_ERASE_64K_3B: u8 = 0xD8;

// 4-byte address variants
const CMD_READ_DATA_4B: u8 = 0x13;
#[allow(dead_code)]
const CMD_FAST_READ_4B: u8 = 0x0C;
const CMD_PAGE_PROGRAM_4B: u8 = 0x12;
const CMD_SECTOR_ERASE_4K_4B: u8 = 0x21;
const CMD_BLOCK_ERASE_32K_4B: u8 = 0x5C;
const CMD_BLOCK_ERASE_64K_4B: u8 = 0xDC;

// Multi-lane fast reads — picked when MpiConfig.preferred_mode = Fastest
// and BFPT advertises the corresponding mode. Single-line opcode
// (instruction always on 1 lane); admode/dmode lanes vary per mode.
const CMD_DUAL_OUTPUT_READ_3B: u8 = 0x3B; // 1-1-2
const CMD_DUAL_OUTPUT_READ_4B: u8 = 0x3C;
const CMD_DUAL_IO_READ_3B: u8 = 0xBB; // 1-2-2
const CMD_DUAL_IO_READ_4B: u8 = 0xBC;
const CMD_QUAD_OUTPUT_READ_3B: u8 = 0x6B; // 1-1-4
const CMD_QUAD_OUTPUT_READ_4B: u8 = 0x6C;
const CMD_QUAD_IO_READ_3B: u8 = 0xEB; // 1-4-4
const CMD_QUAD_IO_READ_4B: u8 = 0xEC;

// Status register 2 ops — needed for QE-bit management before any
// quad-mode read. Chips vary on which is usable (BFPT QER tells us).
const CMD_READ_STATUS_2: u8 = 0x35;
const CMD_WRITE_STATUS_2: u8 = 0x31;
const CMD_WRITE_STATUS_1_2: u8 = 0x01; // WRSR, 2-byte payload = SR1 | SR2

// Status register 1 bits.
const SR1_WIP: u8 = 0x01;
const SR1_WEL: u8 = 0x02;

const PAGE_SIZE: u32 = 256;
const ERASE_4K: u32 = 4096;
const ERASE_32K: u32 = 32 * 1024;
const ERASE_64K: u32 = 64 * 1024;

// SFDP transaction parameters per JESD216:
//   0x5A opcode, single-line instruction
//   3-byte single-line address
//   8 dummy cycles (always, regardless of chip's native mode)
//   single-line data
const SFDP_DUMMY_CYCLES: u8 = 8;

// Poll limits. Sized for worst-case at minimum supported clock.
// Transfer complete / FIFO operations should resolve in microseconds.
const MAX_TRANSFER_POLLS: u32 = 1_000_000;
// FIFO ready should resolve quickly per word.
const MAX_FIFO_POLLS: u32 = 100_000;
// WIP polling: chip erase can take tens of seconds at slow clocks.
// Each WIP poll is itself a full SPI transaction, so this counts
// outer iterations, not raw register reads.
const MAX_WIP_POLLS: u32 = 1_000_000;

#[derive(Debug, Clone, Copy, rcard_log::Format)]
enum HwTimeout {
    Transfer,
    RxFifo,
    TxFifo,
    Wip,
}

impl From<HwTimeout> for MpiOperationError {
    fn from(t: HwTimeout) -> Self {
        match t {
            HwTimeout::Transfer => MpiOperationError::TransferTimeout,
            HwTimeout::RxFifo => MpiOperationError::RxFifoTimeout,
            HwTimeout::TxFifo => MpiOperationError::TxFifoTimeout,
            HwTimeout::Wip => MpiOperationError::WipTimeout,
        }
    }
}

/// Map the subset of HwTimeout that can arise inside an SFDP body
/// read. `HwTimeout::Wip` never fires here — SFDP transactions don't
/// poll WIP. If it ever does, the invariant has been broken elsewhere
/// and we'd rather panic than silently pick a wrong variant.
fn read_param_err_from_hw(t: HwTimeout) -> ReadParameterError {
    match t {
        HwTimeout::Transfer => ReadParameterError::TransferTimeout,
        HwTimeout::RxFifo => ReadParameterError::RxFifoTimeout,
        HwTimeout::TxFifo => ReadParameterError::TxFifoTimeout,
        HwTimeout::Wip => unreachable!("WIP timeout not reachable from SFDP body read"),
    }
}

struct MpiResource {
    index: u8,
    regs: MpiPeri,
    config: MpiConfig,
    /// Address width resolved from BFPT DWORD 1 bits 18:17 + chip
    /// capacity at `open()`. Drives EN4B/EX4B at init, the 3B/4B opcode
    /// choice in every read/write/erase, and the ADSIZE field in CCR1.
    addr_size: AddrSize,
    /// Chip capacity in bytes, derived from BFPT DWORD 2 at `open()`
    /// and used to bounds-check every subsequent address-bearing
    /// operation. `u64` because SFDP's density field can in principle
    /// encode up to 2^63 bits (8 EB).
    capacity_bytes: u64,
    /// Read-path parameters resolved from BFPT + `config.preferred_mode`
    /// at `open()`. Used by every `read()` call without re-parsing.
    read_mode: ResolvedReadMode,
    /// Cached SFDP parameter headers, populated at `open()`. Only the
    /// first `sfdp_nph` entries are valid. Empty (nph=0) means SFDP
    /// was unusable — every `read_parameter` call then returns the
    /// `SfdpUnavailable` error. 16 slots covers every chip we've seen
    /// (real chips populate 1–5); the spec allows up to 256 but chips
    /// exceeding 16 are rejected at `open()`.
    sfdp_headers: [ParameterHeader; 16],
    sfdp_nph: u16,
    /// Cached SFDP global-header fields. Served back via `read_sfdp`.
    sfdp_major: u8,
    sfdp_minor: u8,
    sfdp_access_protocol: u8,
}

/// Default value for the `sfdp_headers` array pre-population. Safe
/// filler — fields are never read when `sfdp_nph` is 0, and valid
/// slots are overwritten before anyone reads them.
const SFDP_HEADER_ZERO: ParameterHeader = ParameterHeader {
    id: ParameterId(0),
    major: 0,
    minor: 0,
    length_dwords: 0,
    pointer: 0,
};

/// Read-path parameters derived once from BFPT + caller preference.
/// Every `read()` call reads every field — no further decoding per op.
/// Opcodes are stored separately for 3-byte and 4-byte variants because
/// the chip's addressing mode may change across open/close cycles; the
/// driver picks which to use at issue time via `addr_size`.
#[derive(Clone, Copy)]
struct ResolvedReadMode {
    /// Opcode to use when `addr_size == ThreeBytes`.
    opcode_3b: u8,
    /// Opcode to use when `addr_size == FourBytes`.
    opcode_4b: u8,
    /// Line count for the address phase.
    admode: LineMode,
    /// Line count for the data phase.
    dmode: LineMode,
    /// Dummy-cycle count between address/mode byte and data.
    dummy_cycles: u8,
    /// Mode-byte ("alternate byte") cycle count — 0 means no mode byte
    /// is driven; non-zero means the peripheral clocks out an extra
    /// byte after address whose bit pattern gates continuous-read mode.
    mode_cycles: u8,
}

impl ResolvedReadMode {
    /// Single-line standard read. Safe on every SPI NOR flash,
    /// including chips without SFDP or mis-configured ones.
    const SINGLE_STANDARD: Self = Self {
        opcode_3b: CMD_READ_DATA_3B,
        opcode_4b: CMD_READ_DATA_4B,
        admode: LineMode::Single,
        dmode: LineMode::Single,
        dummy_cycles: 0,
        mode_cycles: 0,
    };
}

/// Pick the best read mode the chip supports, honoring caller
/// preference. `Single` → always `SINGLE_STANDARD`. `Fastest` → walk
/// the fast-read modes advertised in BFPT in preference order
/// (1-4-4 → 1-1-4 → 1-2-2 → 1-1-2), falling back to single-line
/// standard when nothing faster is advertised.
fn resolve_read_mode(
    bfpt: &sysmodule_mpi_api::sfdp::Bfpt<'_>,
    preference: ModePreference,
) -> ResolvedReadMode {
    match preference {
        ModePreference::Single => ResolvedReadMode::SINGLE_STANDARD,
        ModePreference::Fastest => resolve_fastest(bfpt),
    }
}

/// Same as `resolve_read_mode(Fastest)` but skipping quad modes.
/// Used as the fallback when QE-bit setup fails.
fn resolve_read_mode_no_quad(bfpt: &sysmodule_mpi_api::sfdp::Bfpt<'_>) -> ResolvedReadMode {
    if let Some(m) = try_dual_io(bfpt) {
        return m;
    }
    if let Some(m) = try_dual_output(bfpt) {
        return m;
    }
    ResolvedReadMode::SINGLE_STANDARD
}

fn resolve_fastest(bfpt: &sysmodule_mpi_api::sfdp::Bfpt<'_>) -> ResolvedReadMode {
    if let Some(m) = try_quad_io(bfpt) {
        return m;
    }
    if let Some(m) = try_quad_output(bfpt) {
        return m;
    }
    if let Some(m) = try_dual_io(bfpt) {
        return m;
    }
    if let Some(m) = try_dual_output(bfpt) {
        return m;
    }
    ResolvedReadMode::SINGLE_STANDARD
}

fn try_quad_io(bfpt: &sysmodule_mpi_api::sfdp::Bfpt<'_>) -> Option<ResolvedReadMode> {
    let p = bfpt.quad_io_read()?;
    // BFPT advertises the 3-byte opcode; 4-byte variant is the standard
    // +1 pattern (0xEB → 0xEC) across GD25, Winbond, Macronix.
    Some(ResolvedReadMode {
        opcode_3b: CMD_QUAD_IO_READ_3B,
        opcode_4b: CMD_QUAD_IO_READ_4B,
        admode: LineMode::Quad,
        dmode: LineMode::Quad,
        dummy_cycles: p.dummy_cycles,
        mode_cycles: p.mode_cycles,
    })
}

fn try_quad_output(bfpt: &sysmodule_mpi_api::sfdp::Bfpt<'_>) -> Option<ResolvedReadMode> {
    let p = bfpt.quad_output_read()?;
    Some(ResolvedReadMode {
        opcode_3b: CMD_QUAD_OUTPUT_READ_3B,
        opcode_4b: CMD_QUAD_OUTPUT_READ_4B,
        admode: LineMode::Single,
        dmode: LineMode::Quad,
        dummy_cycles: p.dummy_cycles,
        mode_cycles: p.mode_cycles,
    })
}

fn try_dual_io(bfpt: &sysmodule_mpi_api::sfdp::Bfpt<'_>) -> Option<ResolvedReadMode> {
    let p = bfpt.dual_io_read()?;
    Some(ResolvedReadMode {
        opcode_3b: CMD_DUAL_IO_READ_3B,
        opcode_4b: CMD_DUAL_IO_READ_4B,
        admode: LineMode::Dual,
        dmode: LineMode::Dual,
        dummy_cycles: p.dummy_cycles,
        mode_cycles: p.mode_cycles,
    })
}

fn try_dual_output(bfpt: &sysmodule_mpi_api::sfdp::Bfpt<'_>) -> Option<ResolvedReadMode> {
    let p = bfpt.dual_output_read()?;
    Some(ResolvedReadMode {
        opcode_3b: CMD_DUAL_OUTPUT_READ_3B,
        opcode_4b: CMD_DUAL_OUTPUT_READ_4B,
        admode: LineMode::Single,
        dmode: LineMode::Dual,
        dummy_cycles: p.dummy_cycles,
        mode_cycles: p.mode_cycles,
    })
}

impl MpiResource {
    fn wait_transfer_complete(&self) -> Result<(), HwTimeout> {
        for _ in 0..MAX_TRANSFER_POLLS {
            if self.regs.sr().read().tcf() {
                self.regs.scr().write(|w| w.set_tcfc(true));
                return Ok(());
            }
        }
        Err(HwTimeout::Transfer)
    }

    fn wait_rx_ready(&self) -> Result<(), HwTimeout> {
        for _ in 0..MAX_FIFO_POLLS {
            if !self.regs.fifocr().read().rxe() {
                return Ok(());
            }
        }
        Err(HwTimeout::RxFifo)
    }

    fn wait_tx_ready(&self) -> Result<(), HwTimeout> {
        for _ in 0..MAX_FIFO_POLLS {
            if !self.regs.fifocr().read().txf() {
                return Ok(());
            }
        }
        Err(HwTimeout::TxFifo)
    }

    /// Send a command-only sequence (no address, no data). The driver
    /// only ever uses single-line SPI for operational commands — quad
    /// modes would require chip-specific status-register setup
    /// (typically the QE bit) that isn't implemented here yet. The
    /// `_imode` variant exists for `open()`'s reset sequence, which
    /// fires on both QUAD and SINGLE lines to recover from any
    /// BOOTROM starting state.
    fn cmd_only(&self, instruction: u8) -> Result<(), HwTimeout> {
        self.cmd_only_imode(instruction, LineMode::Single)
    }

    /// Send a command-only sequence with an explicit instruction-line
    /// mode. Used during open()'s reset sequence, which targets both
    /// QUAD and SINGLE lines since the chip's starting mode is unknown.
    fn cmd_only_imode(&self, instruction: u8, imode: LineMode) -> Result<(), HwTimeout> {
        self.regs.ccr1().write(|w| {
            w.set_imode(imode as u8);
        });
        // CMDR1 write triggers the hardware sequence — must come after CCR1
        self.regs.cmdr1().write(|w| w.set_cmd(instruction));
        self.wait_transfer_complete()
    }

    /// Send a command + address, no data. Line modes hardcoded to
    /// single-line (1-1-?); address width comes from the SFDP-resolved
    /// `self.addr_size`.
    fn cmd_addr(&self, instruction: u8, address: u32) -> Result<(), HwTimeout> {
        self.regs.ar1().write(|w| w.0 = address);
        self.regs.ccr1().write(|w| {
            w.set_imode(LineMode::Single as u8);
            w.set_admode(LineMode::Single as u8);
            w.set_adsize(self.addr_size as u8);
        });
        self.regs.cmdr1().write(|w| w.set_cmd(instruction));
        self.wait_transfer_complete()
    }

    /// Read one byte of status register 1. Single-line transaction —
    /// status-reg ops always use 1-1-1 regardless of the resolved read
    /// mode, because SRs aren't addressed and can't use quad modes.
    fn read_status_1(&self) -> Result<u8, HwTimeout> {
        self.regs.dlr1().write(|w| w.0 = 0); // 1 byte (n-1 encoding)
        self.regs.ccr1().write(|w| {
            w.set_imode(LineMode::Single as u8);
            w.set_dmode(LineMode::Single as u8);
        });
        self.regs.cmdr1().write(|w| w.set_cmd(CMD_READ_STATUS_1));
        self.wait_transfer_complete()?;
        self.wait_rx_ready()?;
        Ok(self.regs.dr().read().0 as u8)
    }

    /// Send CMD_WRITE_ENABLE and confirm WEL actually latched. If the
    /// bit doesn't set, the chip rejected the command — usually WP#
    /// pin pulled low, an SRP status-lock, or the chip simply isn't
    /// listening. Any of those mean subsequent program/erase opcodes
    /// would be silently dropped.
    fn write_enable(&self) -> Result<(), MpiOperationError> {
        self.cmd_only(CMD_WRITE_ENABLE)?;
        let sr = self.read_status_1()?;
        if sr & SR1_WEL == 0 {
            error!("MPI{}: WEL did not latch, sr1=0x{:02x}", self.index, sr);
            return Err(MpiOperationError::WriteEnableDidNotLatch);
        }
        Ok(())
    }

    /// Whether the SFDP-resolved addressing uses the 4-byte opcode
    /// family. Set by `init_sfdp` from BFPT advertisements + capacity;
    /// changes only across `open()` calls.
    fn is_4byte(&self) -> bool {
        matches!(self.addr_size, AddrSize::FourBytes)
    }

    /// Read opcode to issue given current addressing width and the
    /// SFDP-resolved read mode. Opcode selection is a mode-vs-addr-size
    /// matrix; the mode-level opcode pair is stored on `ResolvedReadMode`
    /// and we pick 3B or 4B based on `self.addr_size`.
    fn cmd_read(&self) -> u8 {
        if self.is_4byte() {
            self.read_mode.opcode_4b
        } else {
            self.read_mode.opcode_3b
        }
    }

    fn cmd_page_program(&self) -> u8 {
        if self.is_4byte() {
            CMD_PAGE_PROGRAM_4B
        } else {
            CMD_PAGE_PROGRAM_3B
        }
    }

    fn cmd_sector_erase_4k(&self) -> u8 {
        if self.is_4byte() {
            CMD_SECTOR_ERASE_4K_4B
        } else {
            CMD_SECTOR_ERASE_4K_3B
        }
    }

    fn cmd_block_erase_32k(&self) -> u8 {
        if self.is_4byte() {
            CMD_BLOCK_ERASE_32K_4B
        } else {
            CMD_BLOCK_ERASE_32K_3B
        }
    }

    fn cmd_block_erase_64k(&self) -> u8 {
        if self.is_4byte() {
            CMD_BLOCK_ERASE_64K_4B
        } else {
            CMD_BLOCK_ERASE_64K_3B
        }
    }

    /// Poll SR1 until WIP clears.
    fn wait_wip(&self) -> Result<(), HwTimeout> {
        for _ in 0..MAX_WIP_POLLS {
            let sr = self.read_status_1()?;
            if sr & SR1_WIP == 0 {
                return Ok(());
            }
        }
        Err(HwTimeout::Wip)
    }

    /// Populate SFDP state at `open()`: reads the SFDP header + every
    /// parameter header into `self.sfdp_headers`, locates BFPT, and
    /// derives `self.capacity_bytes` from BFPT DWORD 2 for later
    /// bounds-checking. Fails `open()` rather than continuing unbounded
    /// — the capacity number is load-bearing for every read/write/erase.
    fn init_sfdp(&mut self) -> Result<(), MpiOpenError> {
        const HEADER_BYTES: usize = 8;
        const PH_BYTES: usize = 8;
        // read_sfdp_raw caps at 256 bytes per call (per its doc).
        const CHUNK: usize = 256;

        let mut sfdp_header = [0u8; HEADER_BYTES];
        self.read_sfdp_raw(0x00, &mut sfdp_header).map_err(|e| {
            error!("MPI{}: SFDP header read failed: {}", self.index, e);
            MpiOpenError::SfdpUnavailable
        })?;

        if &sfdp_header[0..4] != b"SFDP" {
            error!(
                "MPI{}: SFDP signature invalid: {:02x} {:02x} {:02x} {:02x}",
                self.index, sfdp_header[0], sfdp_header[1], sfdp_header[2], sfdp_header[3]
            );
            return Err(MpiOpenError::SfdpUnavailable);
        }

        let sfdp_minor = sfdp_header[4];
        let sfdp_major = sfdp_header[5];
        // NPH field is stored as "N-1": 0 means one header, 255 means 256.
        let nph_raw = sfdp_header[6] as u16 + 1;
        let access_protocol = sfdp_header[7];
        self.sfdp_major = sfdp_major;
        self.sfdp_minor = sfdp_minor;
        self.sfdp_access_protocol = access_protocol;

        const MAX_NPH: u16 = 16;
        if nph_raw > MAX_NPH {
            error!(
                "MPI{}: SFDP declares {} parameter headers, capped at {}",
                self.index, nph_raw, MAX_NPH
            );
        }
        let nph = nph_raw.min(MAX_NPH);

        // Read parameter headers. 16 * 8 = 128 bytes; single read.
        let ph_bytes = unsafe { &mut *(&raw mut MPI_SCRATCH) };
        let total = (nph as usize) * PH_BYTES;
        let mut cursor = 0usize;
        while cursor < total {
            let take = (total - cursor).min(CHUNK);
            let offset = (HEADER_BYTES + cursor) as u32;
            self.read_sfdp_raw(offset, &mut ph_bytes[cursor..cursor + take])
                .map_err(|e| {
                    error!(
                        "MPI{}: SFDP parameter-header read failed: {}",
                        self.index, e
                    );
                    MpiOpenError::SfdpUnavailable
                })?;
            cursor += take;
        }

        // Decode 8-byte slots into ParameterHeader. Packed-struct layout
        // matters only on the wire; the Rust-side copy is fine.
        for i in 0..nph as usize {
            let s = &ph_bytes[i * PH_BYTES..(i + 1) * PH_BYTES];
            self.sfdp_headers[i] = ParameterHeader {
                id: ParameterId(((s[7] as u16) << 8) | (s[0] as u16)),
                minor: s[1],
                major: s[2],
                length_dwords: s[3],
                pointer: u32::from_le_bytes([s[4], s[5], s[6], 0]),
            };
        }
        self.sfdp_nph = nph;

        debug!(
            "MPI{}: SFDP rev={}.{} nph={} access=0x{:02x}",
            self.index, sfdp_major, sfdp_minor, nph, access_protocol
        );

        // Locate BFPT (should always be index 0 per spec, but search by
        // ID so malformed ordering doesn't brick us). Derive capacity
        // from its DWORD 2.
        let bfpt = *self.sfdp_headers[..nph as usize]
            .iter()
            .find(|ph| ph.id == ParameterId::BFPT)
            .ok_or_else(|| {
                error!("MPI{}: BFPT missing from SFDP", self.index);
                MpiOpenError::SfdpUnavailable
            })?;

        let bfpt_len_dwords = bfpt.length_dwords;
        let bfpt_pointer = bfpt.pointer;
        if (bfpt_len_dwords as usize) < 2 {
            error!(
                "MPI{}: BFPT too short for density: {} dwords",
                self.index, bfpt_len_dwords
            );
            return Err(MpiOpenError::SfdpUnavailable);
        }

        // Read the whole BFPT body — we need DWORDs 1 and 2 for
        // address-width + density, and may later consume DWORDs 3-4
        // (fast-read triples) and 8-9 (erase types). 256 bytes covers
        // rev-F (23 DWORDs = 92 bytes) with headroom. Real BFPTs are
        // 36–92 bytes; anything larger is rejected.
        const MAX_BFPT: usize = 256;
        let body_len = (bfpt_len_dwords as usize) * 4;
        if body_len > MAX_BFPT {
            error!(
                "MPI{}: BFPT declared too large: {} bytes",
                self.index, body_len
            );
            return Err(MpiOpenError::SfdpUnavailable);
        }
        let bfpt_buf = unsafe { &mut *(&raw mut MPI_SCRATCH) };
        let mut cursor = 0usize;
        while cursor < body_len {
            let take = (body_len - cursor).min(CHUNK);
            self.read_sfdp_raw(
                bfpt_pointer + cursor as u32,
                &mut bfpt_buf[cursor..cursor + take],
            )
            .map_err(|e| {
                error!("MPI{}: BFPT body read failed: {}", self.index, e);
                MpiOpenError::SfdpUnavailable
            })?;
            cursor += take;
        }
        let bfpt = sysmodule_mpi_api::sfdp::Bfpt::new(&bfpt_buf[..body_len]);

        let capacity_bytes = bfpt.density_bytes().ok_or_else(|| {
            error!("MPI{}: BFPT density field invalid", self.index);
            MpiOpenError::SfdpUnavailable
        })?;
        self.capacity_bytes = capacity_bytes;

        // Resolve addressing width from BFPT DWORD 1 bits 18:17 paired
        // with density. For chips advertising both 3-byte and 4-byte
        // support, we pick 4-byte when the chip won't otherwise fit —
        // this is a one-way decision; `open()` will then issue the
        // matching EN4B / EX4B to enforce it.
        let addr_support = bfpt.address_bytes().ok_or_else(|| {
            error!("MPI{}: BFPT address_bytes field unreadable", self.index);
            MpiOpenError::SfdpUnavailable
        })?;
        use sysmodule_mpi_api::sfdp::AddrBytesSupport;
        let addr_size = match addr_support {
            AddrBytesSupport::ThreeOnly => AddrSize::ThreeBytes,
            AddrBytesSupport::FourOnly => AddrSize::FourBytes,
            AddrBytesSupport::ThreeOrFour => {
                // 16 MiB is the 3-byte-address ceiling; above it, a
                // 3-byte-only mode can't reach the top half of the chip.
                if capacity_bytes > 16 * 1024 * 1024 {
                    AddrSize::FourBytes
                } else {
                    AddrSize::ThreeBytes
                }
            }
            AddrBytesSupport::Reserved => {
                error!("MPI{}: BFPT address-width encoding is Reserved", self.index);
                return Err(MpiOpenError::SfdpUnavailable);
            }
        };
        self.addr_size = addr_size;

        // Resolve read mode against caller preference + BFPT
        // advertisements. For quad modes, also set the chip's QE bit
        // before the first read — chips silently drop quad commands if
        // QE is clear. If anything in the quad-enable path fails we
        // degrade to the best non-quad mode so `open()` still succeeds.
        self.read_mode = resolve_read_mode(&bfpt, self.config.preferred_mode);
        if self.read_mode.dmode == LineMode::Quad {
            if let Err(e) = self.ensure_qe_bit(&bfpt) {
                error!(
                    "MPI{}: QE-bit setup failed ({}); falling back to non-quad read mode",
                    self.index, e
                );
                self.read_mode = resolve_read_mode_no_quad(&bfpt);
            }
        }

        debug!(
            "MPI{}: SFDP resolved: capacity={} bytes, addr_size={}, read_opcode={}/{}, admode={}, dmode={}, dummy={}, mode_cycles={}",
            self.index,
            capacity_bytes,
            addr_size,
            self.read_mode.opcode_3b,
            self.read_mode.opcode_4b,
            self.read_mode.admode,
            self.read_mode.dmode,
            self.read_mode.dummy_cycles,
            self.read_mode.mode_cycles,
        );

        Ok(())
    }

    /// Ensure the chip's QE ("quad enable") bit is set so quad-mode
    /// reads actually respond. Uses BFPT's QER advertisement to pick
    /// the right status-register location and write sequence. Returns
    /// `MpiOperationError` on any underlying SPI / WEL failure; the
    /// caller is expected to degrade to a non-quad mode on error.
    fn ensure_qe_bit(
        &self,
        bfpt: &sysmodule_mpi_api::sfdp::Bfpt<'_>,
    ) -> Result<(), MpiOperationError> {
        use sysmodule_mpi_api::sfdp::QuadEnableRequirement as QER;
        // Rev-A chips don't have DWORD 15; assume the most common
        // modern encoding (SR2 bit 1, WRSR 2-byte) as a default.
        let qer = bfpt
            .quad_enable_requirement()
            .unwrap_or(QER::Sr2Bit1Wrsr2Byte);

        match qer {
            QER::None | QER::Reserved => {
                // Nothing known; hope the chip is already quad-capable.
                trace!("MPI{}: QER=None/Reserved, skipping QE write", self.index);
                Ok(())
            }
            QER::Sr1Bit6 => self.set_qe_bit_sr1(1 << 6),
            QER::Sr2Bit7 => self.set_qe_bit_sr2_only(1 << 7, CMD_WRITE_STATUS_2),
            QER::Sr2Bit1Wrsr2 | QER::Sr2Bit1Wrsr2Alt => {
                self.set_qe_bit_sr2_only(1 << 1, CMD_WRITE_STATUS_2)
            }
            QER::Sr2Bit1Wrsr2Byte | QER::Sr2Bit1WrsrMixed => self.set_qe_bit_sr2_wrsr2byte(),
        }
    }

    /// Set a bit in SR1 via WRSR (0x01), 1-byte payload.
    fn set_qe_bit_sr1(&self, bit_mask: u8) -> Result<(), MpiOperationError> {
        let sr1 = self.read_status_1()?;
        if sr1 & bit_mask != 0 {
            return Ok(());
        }
        self.write_enable()?;
        // WRSR (0x01) with 1 byte = updated SR1.
        self.write_one_byte(CMD_WRITE_STATUS_1_2, sr1 | bit_mask)?;
        self.wait_wip()?;
        Ok(())
    }

    /// Set a bit in SR2 via a 1-byte opcode (either WRSR2=0x31 or
    /// another chip-specific WRSR2 variant, passed by the caller).
    fn set_qe_bit_sr2_only(&self, bit_mask: u8, write_opcode: u8) -> Result<(), MpiOperationError> {
        let sr2 = self.read_status_2()?;
        if sr2 & bit_mask != 0 {
            return Ok(());
        }
        self.write_enable()?;
        self.write_one_byte(write_opcode, sr2 | bit_mask)?;
        self.wait_wip()?;
        Ok(())
    }

    /// Set SR2 bit 1 via WRSR (0x01) with a 2-byte payload (SR1 then
    /// SR2). Used for chips whose QER = 0b100 or 0b101.
    fn set_qe_bit_sr2_wrsr2byte(&self) -> Result<(), MpiOperationError> {
        let sr2 = self.read_status_2()?;
        if sr2 & (1 << 1) != 0 {
            return Ok(());
        }
        let sr1 = self.read_status_1()?;
        self.write_enable()?;
        // WRSR (0x01) with 2 bytes. Driver packs 1 word = SR1 | (SR2 << 8).
        let payload: u32 = (sr1 as u32) | (((sr2 | (1 << 1)) as u32) << 8);
        self.write_two_bytes(CMD_WRITE_STATUS_1_2, payload)?;
        self.wait_wip()?;
        Ok(())
    }

    /// Send a single-byte write command: opcode followed by 1 data byte.
    fn write_one_byte(&self, instruction: u8, data: u8) -> Result<(), HwTimeout> {
        self.regs.dlr1().write(|w| w.0 = 0); // 1 byte (n-1 encoding)
        self.regs.ccr1().write(|w| {
            w.set_imode(LineMode::Single as u8);
            w.set_dmode(LineMode::Single as u8);
            w.set_fmode(true);
        });
        self.regs.cmdr1().write(|w| w.set_cmd(instruction));
        self.wait_tx_ready()?;
        self.regs.dr().write(|w| w.0 = data as u32);
        self.wait_transfer_complete()
    }

    /// Send a two-byte write command (`payload` = byte0 | (byte1 << 8)).
    fn write_two_bytes(&self, instruction: u8, payload: u32) -> Result<(), HwTimeout> {
        self.regs.dlr1().write(|w| w.0 = 1); // 2 bytes (n-1 encoding)
        self.regs.ccr1().write(|w| {
            w.set_imode(LineMode::Single as u8);
            w.set_dmode(LineMode::Single as u8);
            w.set_fmode(true);
        });
        self.regs.cmdr1().write(|w| w.set_cmd(instruction));
        self.wait_tx_ready()?;
        self.regs.dr().write(|w| w.0 = payload);
        self.wait_transfer_complete()
    }

    /// Read SR2 via 0x35.
    fn read_status_2(&self) -> Result<u8, HwTimeout> {
        self.regs.dlr1().write(|w| w.0 = 0);
        self.regs.ccr1().write(|w| {
            w.set_imode(LineMode::Single as u8);
            w.set_dmode(LineMode::Single as u8);
        });
        self.regs.cmdr1().write(|w| w.set_cmd(CMD_READ_STATUS_2));
        self.wait_transfer_complete()?;
        self.wait_rx_ready()?;
        Ok(self.regs.dr().read().0 as u8)
    }

    /// Read SFDP bytes at `offset` into `buf` using the spec-mandated
    /// single-line + 8-dummy transaction. `buf.len()` must be ≤ 256.
    fn read_sfdp_raw(&self, offset: u32, buf: &mut [u8]) -> Result<(), HwTimeout> {
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }
        self.regs.dlr1().write(|w| w.0 = (len - 1) as u32);
        self.regs.ar1().write(|w| w.0 = offset);
        self.regs.ccr1().write(|w| {
            w.set_imode(LineMode::Single as u8);
            w.set_admode(LineMode::Single as u8);
            w.set_adsize(AddrSize::ThreeBytes as u8);
            w.set_dmode(LineMode::Single as u8);
            w.set_dcyc(SFDP_DUMMY_CYCLES);
        });
        self.regs.cmdr1().write(|w| w.set_cmd(CMD_SFDP));

        let mut i = 0;
        while i < len {
            self.wait_rx_ready()?;
            let word = self.regs.dr().read().0;
            for byte_idx in 0..4 {
                if i < len {
                    buf[i] = (word >> (byte_idx * 8)) as u8;
                    i += 1;
                }
            }
        }
        self.wait_transfer_complete()?;
        Ok(())
    }
}

impl Mpi for MpiResource {
    fn open(meta: ipc::Meta, index: u8, config: MpiConfig) -> Result<Self, MpiOpenError> {
        debug!("MPI{}: opening", index);
        let Some(regs) = mpi_instance(index) else {
            error!("MPI{}: invalid index", index);
            return Err(MpiOpenError::InvalidIndex);
        };

        if MPI_IN_USE[(index - 1) as usize].swap(true, Ordering::Acquire) {
            error!("MPI{}: already open", index);
            return Err(MpiOpenError::AlreadyOpen);
        }

        let mut resource = MpiResource {
            index,
            regs,
            config,
            // Safe defaults: if anything slips between field init and
            // `init_sfdp`, any address-bearing op with non-zero capacity
            // will still produce deterministic behavior. SINGLE_STANDARD
            // is the slowest but most compatible read mode and works on
            // any SPI NOR.
            addr_size: AddrSize::ThreeBytes,
            capacity_bytes: 0,
            read_mode: ResolvedReadMode::SINGLE_STANDARD,
            sfdp_headers: [SFDP_HEADER_ZERO; 16],
            sfdp_nph: 0,
            sfdp_major: 0,
            sfdp_minor: 0,
            sfdp_access_protocol: 0,
        };

        // Full RCC block-reset of the MPI peripheral. The stub that
        // precedes us may have left it mid-transaction, mid-QPI-setup,
        // or with arbitrary FIFO / IRQ state asserted; a block reset
        // is the only clean recovery.
        let rcc = sifli_pac::HPSYS_RCC;
        match index {
            1 => {
                rcc.rstr2().modify(|w| w.set_mpi1(true));
                core::sync::atomic::compiler_fence(Ordering::SeqCst);
                rcc.rstr2().modify(|w| w.set_mpi1(false));
                rcc.enr2().modify(|w| w.set_mpi1(true));
            }
            2 => {
                rcc.rstr2().modify(|w| w.set_mpi2(true));
                core::sync::atomic::compiler_fence(Ordering::SeqCst);
                rcc.rstr2().modify(|w| w.set_mpi2(false));
                rcc.enr2().modify(|w| w.set_mpi2(true));
            }
            _ => unreachable!("index validated above"),
        }

        // Reapply init. TIMR/CIR/ABR1/HRABR mirror the SDK's
        // `HAL_QSPI_Init` (drivers/hal/bf0_hal_qspi.c). CIR is the
        // "command interval" — minimum gap between back-to-back
        // commands. Default 0 confuses the chip during init while
        // it's still settling from a wake-from-DP; the SDK uses
        // 0x5000 in both halves (~85 µs at 240 MHz).
        //
        // ABR1=0xFF is the mode byte driven during the alternate-byte
        // phase of fast-IO reads. Bits 5:4 = 11, which is the
        // "not-CRM" encoding on Winbond W25Q, GD25, and Macronix MX25
        // — the chips we've validated. Other vendors (Cypress/Infineon,
        // ISSI, some EON) may use a different CRM-exit encoding, in
        // which case a quad-IO read could leave the chip in CRM and
        // misinterpret the next instruction byte as a mode byte. The
        // reset sequence issued by `open()` clears any stuck CRM
        // state, but steady-state operation on untested vendors is
        // not guaranteed. Set `preferred_mode = Single` in `MpiConfig`
        // if bringing up a new vendor whose CRM encoding you haven't
        // verified.
        resource.regs.timr().write(|w| w.0 = 0xF);
        resource.regs.cir().write(|w| w.0 = 0x5000_5000);
        resource.regs.abr1().write(|w| w.0 = 0xFF);
        resource.regs.hrabr().write(|w| w.0 = 0xFF);
        resource.regs.psclr().write(|w| w.set_div(config.prescaler));
        // Write (not modify) MISCR so RXCLKINV / RXCLKDLY / DTRPRE /
        // SCKDLY all land at 0 regardless of what BOOTROM put there.
        // Per SDK comment (bf0_hal_mpi.h:54): on 52x, RXCLKINV=1 with
        // 3.3V sip flash makes JEDEC ID reads fail — and RSTR2.mpi2
        // may not cover that bit.
        resource.regs.miscr().write(|w| {
            w.set_sckinv(config.clock_polarity as u8 != 0);
        });
        resource.regs.cr().write(|w| w.set_en(true));

        // Wake + reset, once on QUAD lines then once on SINGLE lines.
        // BOOTROM may have left the chip in any of { SPI, QPI } ×
        // { awake, DPD }:
        //
        //   QPI + DPD   → QUAD RDPD wakes it; QUAD RST_EN+RST returns
        //                 the chip to SPI mode.
        //   QPI + awake → QUAD RDPD is a no-op; QUAD RST_EN+RST returns
        //                 the chip to SPI.
        //   SPI + DPD   → QUAD pass is bit-garbage on DIO0 and ignored;
        //                 SINGLE RDPD wakes the chip; SINGLE RST_EN+RST
        //                 scrubs any residual status-reg state.
        //   SPI + awake → every command is a no-op or idempotent reset.
        //
        // Dropping either pass leaves a start state uncovered, so run
        // both. Commands sent on the wrong line-count get interpreted
        // as 2-clock fragments by a non-QPI chip, or as bit garbage by
        // a QPI chip; both are aborted mid-instruction with no side
        // effect, so blasting them blind is safe.
        //
        // After each pass we busy-wait a tRST margin. SPI NOR `tRST`
        // is spec'd at ~30 µs (Winbond W25Q, GD25, Macronix MX25);
        // during that window the chip ignores all commands, so the
        // first command of the next pass would be silently dropped if
        // issued too early. 50_000 spin_loop iterations is ~200 µs at
        // 240 MHz — well above tRST with margin for slower clocks.
        for imode in [LineMode::Quad, LineMode::Single] {
            for &cmd in &[CMD_RELEASE_DPD, CMD_RESET_ENABLE, CMD_RESET] {
                if let Err(e) = resource.cmd_only_imode(cmd, imode) {
                    // Timing out on the mode that doesn't match the
                    // chip's starting state is expected — log at
                    // trace, not error.
                    trace!(
                        "MPI{}: reset cmd 0x{:02x} on {} lines: {}",
                        index,
                        cmd,
                        imode,
                        e
                    );
                }
            }
            for _ in 0..50_000 {
                core::hint::spin_loop();
            }
        }

        // Confirm the chip is alive on single-line SPI post-reset. JEDEC
        // is a sanity check only — capacity comes from SFDP below.
        let jedec = match resource.read_jedec_id(meta) {
            Ok(j) => j,
            Err(e) => {
                error!("MPI{}: failed to read JEDEC ID after reset: {}", index, e);
                return Err(MpiOpenError::JedecReadFailed);
            }
        };

        if !jedec.is_responding() {
            error!("MPI{}: JEDEC not responding: {}", index, jedec);
            return Err(MpiOpenError::ChipNotResponding);
        }

        // Populate SFDP state and derive capacity. Must run before any
        // address-bearing operation (read/write/erase) — capacity
        // bounds-checks every one of those.
        resource.init_sfdp()?;

        debug!(
            "MPI{}: opened, jedec={}, capacity={} bytes",
            index, jedec, resource.capacity_bytes
        );

        // Align the chip's addressing mode with the SFDP-resolved
        // `resource.addr_size`. EN4B/EX4B are command-only and
        // idempotent — safe to issue even if the chip is already in
        // the target mode. OneByte / TwoBytes aren't real SPI NOR
        // modes, so we don't touch the chip's mode bit for those.
        //
        // Propagate failure: if EN4B fails but we've committed to
        // `FourBytes`, subsequent reads/writes/erases would issue
        // 4-byte opcodes to a chip still in 3-byte mode, interpreting
        // the 4th address byte as data — silent corruption.
        match resource.addr_size {
            AddrSize::FourBytes => {
                if let Err(e) = resource.cmd_only(CMD_ENTER_4BYTE_MODE) {
                    error!("MPI{}: EN4B failed: {}", index, e);
                    return Err(MpiOpenError::AddressModeSwitchFailed);
                }
            }
            AddrSize::ThreeBytes => {
                if let Err(e) = resource.cmd_only(CMD_EXIT_4BYTE_MODE) {
                    error!("MPI{}: EX4B failed: {}", index, e);
                    return Err(MpiOpenError::AddressModeSwitchFailed);
                }
            }
            _ => {}
        }

        Ok(resource)
    }

    fn read_jedec_id(&mut self, _meta: ipc::Meta) -> Result<JedecId, MpiOperationError> {
        self.regs.dlr1().write(|w| w.0 = 2); // 3 bytes (n-1 encoding)
                                             // JEDEC ID is always read single-line regardless of the chip's
                                             // configured fast-read mode — it's the pre-negotiation ID read.
        self.regs.ccr1().write(|w| {
            w.set_imode(LineMode::Single as u8);
            w.set_dmode(LineMode::Single as u8);
        });
        self.regs.cmdr1().write(|w| w.set_cmd(CMD_READ_JEDEC_ID));
        self.wait_transfer_complete()?;
        self.wait_rx_ready()?;
        let raw = self.regs.dr().read().0;
        Ok(JedecId::new(raw as u8, (raw >> 8) as u8, (raw >> 16) as u8))
    }

    fn read_sfdp(
        &mut self,
        _meta: ipc::Meta,
        lease: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<SfdpHeader, ReadSfdpError> {
        if self.sfdp_nph == 0 {
            return Err(ReadSfdpError::SfdpUnavailable);
        }

        // Reserialize cached parameter headers in on-wire format (8
        // bytes/slot). Only write complete slots — a lease smaller
        // than `nph * 8` is truncated to the largest multiple of 8
        // that fits, so the client never sees a partial final header
        // it can't distinguish from a full one.
        const PH_BYTES: usize = 8;
        let slots_fit = lease.len() / PH_BYTES;
        let slots_to_write = slots_fit.min(self.sfdp_nph as usize);
        for i in 0..slots_to_write {
            let ph = self.sfdp_headers[i];
            let id_val: u16 = ph.id.0;
            let pointer: u32 = ph.pointer;
            let bytes: [u8; PH_BYTES] = [
                (id_val & 0xFF) as u8,
                ph.minor,
                ph.major,
                ph.length_dwords,
                (pointer & 0xFF) as u8,
                ((pointer >> 8) & 0xFF) as u8,
                ((pointer >> 16) & 0xFF) as u8,
                ((id_val >> 8) & 0xFF) as u8,
            ];
            lease.write_range(i * PH_BYTES, &bytes).log_unwrap();
        }

        Ok(SfdpHeader {
            major: self.sfdp_major,
            minor: self.sfdp_minor,
            nph: self.sfdp_nph,
            access_protocol: self.sfdp_access_protocol,
        })
    }

    fn read(
        &mut self,
        _meta: ipc::Meta,
        address: u32,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<(), MpiOperationError> {
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }
        if len > PAGE_SIZE as usize {
            return Err(MpiOperationError::LengthTooLarge);
        }
        let end = address
            .checked_add(len as u32)
            .ok_or(MpiOperationError::AddressOutOfRange)?;
        if end as u64 > self.capacity_bytes {
            return Err(MpiOperationError::AddressOutOfRange);
        }

        self.regs.dlr1().write(|w| w.0 = (len - 1) as u32);
        self.regs.ar1().write(|w| w.0 = address);
        // Instruction is always single-line (no QPI in this driver).
        // Address + data lanes come from the SFDP-resolved mode. For
        // modes that specify a mode-byte ("alternate byte") cycle
        // count, we drive ABR1 via the alternate-byte phase; our
        // open() initialized ABR1 to 0xFF (bits 5:4 = 11, which is
        // *not* the 2'b10 "stay in CRM" encoding — so every read
        // transaction leaves the chip in plain-SPI mode afterwards).
        self.regs.ccr1().write(|w| {
            w.set_imode(LineMode::Single as u8);
            w.set_admode(self.read_mode.admode as u8);
            w.set_adsize(self.addr_size as u8);
            w.set_dmode(self.read_mode.dmode as u8);
            w.set_dcyc(self.read_mode.dummy_cycles);
            if self.read_mode.mode_cycles > 0 {
                // Alternate-byte phase drives the mode byte on the same
                // lanes as the address phase. `absize` is stored as
                // (byte_count - 1) so a single byte = 0.
                w.set_abmode(self.read_mode.admode as u8);
                w.set_absize(0);
            }
        });
        self.regs.cmdr1().write(|w| w.set_cmd(self.cmd_read()));

        let local = unsafe { &mut *(&raw mut MPI_SCRATCH) };
        let mut i = 0;
        while i < len {
            self.wait_rx_ready()?;
            let word = self.regs.dr().read().0;
            for byte_idx in 0..4 {
                if i < len {
                    local[i] = (word >> (byte_idx * 8)) as u8;
                    i += 1;
                }
            }
        }

        self.wait_transfer_complete()?;
        buf.write_range(0, &local[..len]).log_unwrap();
        Ok(())
    }

    fn write(
        &mut self,
        _meta: ipc::Meta,
        address: u32,
        data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<(), MpiOperationError> {
        let total = data.len();
        if total == 0 {
            return Ok(());
        }
        let end = address
            .checked_add(total as u32)
            .ok_or(MpiOperationError::AddressOutOfRange)?;
        if end as u64 > self.capacity_bytes {
            return Err(MpiOperationError::AddressOutOfRange);
        }

        let local = unsafe { &mut *(&raw mut MPI_SCRATCH) };
        let mut offset: usize = 0;
        let mut addr = address;

        while offset < total {
            let page_remaining = (PAGE_SIZE - (addr % PAGE_SIZE)) as usize;
            let chunk = core::cmp::min(total - offset, page_remaining);

            data.read_range(offset, &mut local[..chunk]).log_unwrap();

            self.write_enable()?;

            self.regs.dlr1().write(|w| w.0 = (chunk - 1) as u32);
            self.regs.ar1().write(|w| w.0 = addr);
            // Page program is always single-line (1-1-1) on this
            // driver — chips with quad program support (1-4-4 PP) need
            // additional setup we don't expose yet, and single-line PP
            // is universally supported at no functional cost.
            self.regs.ccr1().write(|w| {
                w.set_imode(LineMode::Single as u8);
                w.set_admode(LineMode::Single as u8);
                w.set_adsize(self.addr_size as u8);
                w.set_dmode(LineMode::Single as u8);
                w.set_fmode(true); // write mode
            });
            self.regs
                .cmdr1()
                .write(|w| w.set_cmd(self.cmd_page_program()));

            let mut i = 0;
            while i < chunk {
                let mut word: u32 = 0;
                for byte_idx in 0..4 {
                    if i < chunk {
                        word |= (local[i] as u32) << (byte_idx * 8);
                        i += 1;
                    }
                }
                self.wait_tx_ready()?;
                self.regs.dr().write(|w| w.0 = word);
            }

            self.wait_transfer_complete()?;
            self.wait_wip()?;

            offset += chunk;
            addr += chunk as u32;
        }
        Ok(())
    }

    fn erase(
        &mut self,
        _meta: ipc::Meta,
        address: u32,
        length: u32,
    ) -> Result<(), MpiOperationError> {
        debug!(
            "MPI{}: erase address={} length={}",
            self.index, address, length
        );

        if address % ERASE_4K != 0 {
            return Err(MpiOperationError::InvalidAddressAlignment);
        }
        if length % ERASE_4K != 0 {
            return Err(MpiOperationError::InvalidLengthAlignment);
        }
        let end = address
            .checked_add(length)
            .ok_or(MpiOperationError::AddressOutOfRange)?;
        if end as u64 > self.capacity_bytes {
            return Err(MpiOperationError::AddressOutOfRange);
        }

        let mut addr = address;
        while addr < end {
            let remaining = end - addr;
            let (cmd, step, label) = if addr % ERASE_64K == 0 && remaining >= ERASE_64K {
                (self.cmd_block_erase_64k(), ERASE_64K, "64K")
            } else if addr % ERASE_32K == 0 && remaining >= ERASE_32K {
                (self.cmd_block_erase_32k(), ERASE_32K, "32K")
            } else {
                (self.cmd_sector_erase_4k(), ERASE_4K, "4K")
            };
            trace!("MPI{}: erasing {} block at {}", self.index, label, addr);

            self.write_enable()?;
            self.cmd_addr(cmd, addr)?;
            self.wait_wip()?;
            addr += step;
        }
        Ok(())
    }

    fn erase_chip(&mut self, _meta: ipc::Meta) -> Result<(), MpiOperationError> {
        debug!("MPI{}: erase_chip", self.index);
        self.write_enable()?;
        self.cmd_only(CMD_CHIP_ERASE)?;
        self.wait_wip()?;
        Ok(())
    }

    fn read_parameter(
        &mut self,
        _meta: ipc::Meta,
        id: ParameterId,
        index: u8,
        lease: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<Option<ParameterMetadata>, ReadParameterError> {
        if self.sfdp_nph == 0 {
            return Err(ReadParameterError::SfdpUnavailable);
        }

        // Walk the cached parameter headers once: find the entry at
        // `index` and count total occurrences. The count goes back with
        // the metadata so callers can iterate duplicates without
        // knowing upfront.
        let mut count: u8 = 0;
        let mut found: Option<ParameterHeader> = None;
        for ph in &self.sfdp_headers[..self.sfdp_nph as usize] {
            if ph.id == id {
                if count == index {
                    found = Some(*ph);
                }
                count = count.saturating_add(1);
            }
        }

        let header = match found {
            Some(h) => h,
            // `id` is absent, or caller walked past the last duplicate.
            None => return Ok(None),
        };

        let lease_len = lease.len();
        if lease_len > 0 {
            // Copy into a local staging buffer and then into the lease.
            // read_sfdp_raw caps at 256 bytes per call so we chunk.
            let body_len = (header.length_dwords as usize) * 4;
            let n = body_len.min(lease_len);
            const CHUNK: usize = 256;
            let stage = unsafe { &mut *(&raw mut MPI_SCRATCH) };
            let pointer = header.pointer;
            let mut done = 0usize;
            while done < n {
                let take = (n - done).min(CHUNK);
                self.read_sfdp_raw(pointer + done as u32, &mut stage[..take])
                    .map_err(read_param_err_from_hw)?;
                lease.write_range(done, &stage[..take]).log_unwrap();
                done += take;
            }
        }

        Ok(Some(ParameterMetadata { header, count }))
    }
}

impl Drop for MpiResource {
    fn drop(&mut self) {
        debug!("MPI{}: closing", self.index);
        // Best-effort: wait for any in-flight transfer before cleanup.
        if let Err(e) = self.wait_transfer_complete() {
            error!("MPI cleanup: transfer timeout: {}", e);
        }
        // Unconditional cleanup — safe regardless of hardware state.
        self.regs.fifocr().write(|w| {
            w.set_rxclr(true);
            w.set_txclr(true);
        });
        self.regs.scr().write(|w| w.set_tcfc(true));
        self.regs.cr().write(|w| w.set_en(false));
        MPI_IN_USE[(self.index - 1) as usize].store(false, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// PSRAM bring-up (MPI1)
// ---------------------------------------------------------------------------
//
// The SF32LB525UC6 packages 8 MB of QSPI PSRAM in the same die as the MCU,
// wired to MPI1. Until the controller is configured for PSRAM mode, every
// access to the memory-mapped PSRAM region (0x10000000 / 0x60000000) stalls
// the bus indefinitely on real hardware — emulators paper over this because
// the address range is just regular RAM there.
//
// `init_psram` runs once at boot, before this sysmodule enters its IPC
// server loop, and configures MPI1 for memory-mapped XIP read/write to the
// SiP PSRAM. After it returns, the CPU may freely read/write the PSRAM
// address ranges declared in `firmware/chips/sf32lb525uc6.ncl`.
//
// Boot ordering: this sysmodule runs at `core_sysmodule` priority so the
// kernel scheduler runs it to completion of the synchronous init below
// before scheduling any priority-2 sysmodule (compositor, etc.) that
// touches PSRAM.

// SiP PSRAM (OPI / Octal DDR) opcodes. From the SiFli SDK's
// `bf0_hal_mpi_ex.h` (`HAL_OPSRAM_*`) and `bf0_hal_mpi_psram.c`
// (HAL_OPI_PSRAM_Init / HAL_PSRAM_RESET / HAL_MPI_PSRAM_Init dispatched
// from `bsp_psramc_init`). The part is wired as Octal DDR, NOT QSPI.
const CMD_PSRAM_RESET: u8 = 0xFF; // Global reset
const CMD_PSRAM_MR_READ: u8 = 0x40; // Mode Register Read
const CMD_PSRAM_MR_WRITE: u8 = 0xC0; // Mode Register Write
const CMD_PSRAM_SYNC_READ: u8 = 0x00; // Linear Burst Read (XIP)
const CMD_PSRAM_SYNC_WRITE: u8 = 0x80; // Linear Burst Write (XIP)

/// Mode-register read latency, in dummy cycles. Per the SDK's
/// `HAL_MPI_MR_READ`: `rdcyc = hflash->ecc_en` which is set to 3 in the
/// `freq <= 24 MHz` branch of `HAL_OPI_PSRAM_Init`. CCR1.dcyc is
/// `rdcyc - 1`.
const PSRAM_MR_READ_DCYC: u8 = 2;

// Wire-mode encoding for the controller's IMODE/ADMODE/DMODE/ABMODE
// fields. Value 7 ("quad lines DDR") combined with `CR.OPIE = 1`
// becomes Octal DDR — there is no separate "octal" enum value.
const MODE_OCTAL_DDR: u8 = 7;

// CR.OPIE lives at bit 21, NOT bit 16 as the user manual table suggests.
// The SDK confirms via `cr |= 0x200000` in HAL_FLASH_ENABLE_OPI.
const CR_OPIE: u32 = 1 << 21;

// Read/write latency cycles for the slowest SDK clock branch
// (PSRAM clock ≤ 24 MHz, fix_lat=1). HRCCR/HWCCR.dcyc = lat - 1.
// At higher clocks the SDK bumps these (8/4 at 60 MHz, 14/7 at 84 MHz).
const PSRAM_R_LAT: u8 = 6;
const PSRAM_W_LAT: u8 = 3;

// Mode-register values that match `r_lat=6, w_lat=3, fix_lat=1`. Computed
// per HAL_MPI_SET_FIXLAT (rlat_arr/wlat_arr table lookup):
//   MR0 = (1<<5) | (rlat_arr[r_lat/2] << 2) | 1
//       = (1<<5) | (rlat_arr[3] << 2) | 1
//       = 0x20 | (0 << 2) | 1
//       = 0x21
//   MR4 = (wlat_arr[w_lat] << 5)
//       = (wlat_arr[3] << 5)
//       = 0
const PSRAM_MR0_FIXLAT: u8 = 0x21;
const PSRAM_MR4_FIXLAT: u8 = 0x00;

/// Spin a rough number of microseconds. The existing flash-reset wait
/// (line 941) calibrates `50_000` iterations to ~200 µs at 240 MHz, so
/// 250 iterations per µs is the conservative-margin ratio.
fn spin_us(us: u32) {
    for _ in 0..(us as u64 * 250) {
        core::hint::spin_loop();
    }
}

/// Poll for transfer-complete on the given MPI instance. Mirrors
/// `MpiResource::wait_transfer_complete` (line 311) but operates on the
/// raw `MpiPeri` so we can use it without an `MpiResource`.
fn wait_psram_tcf(regs: MpiPeri) {
    for _ in 0..MAX_TRANSFER_POLLS {
        if regs.sr().read().tcf() {
            regs.scr().write(|w| w.set_tcfc(true));
            return;
        }
    }
    trace!("PSRAM init: tcf timeout (chip in unexpected state for command)");
}

/// Send a one-shot OPI mode-register write to the PSRAM. Mirrors the SDK's
/// `HAL_MPI_MR_WRITE`: 2-byte payload (low byte = value, high byte don't-care),
/// 4-byte mode-register address, all phases on octal DDR.
fn psram_mr_write(regs: MpiPeri, mr_addr: u32, value: u8) {
    regs.dlr1().write(|w| w.set_dlen(1)); // n-1 encoding → 2 bytes
    regs.dr().write(|w| w.0 = value as u32);
    regs.ar1().write(|w| w.0 = mr_addr);
    regs.ccr1().write(|w| {
        w.set_imode(MODE_OCTAL_DDR);
        w.set_admode(MODE_OCTAL_DDR);
        w.set_adsize(3);
        w.set_abmode(0);
        w.set_dmode(MODE_OCTAL_DDR);
        w.set_dcyc(0);
        w.set_fmode(true);
    });
    regs.cmdr1().write(|w| w.set_cmd(CMD_PSRAM_MR_WRITE));
    wait_psram_tcf(regs);
}

/// Read one PSRAM mode register. Mirrors the SDK's `HAL_MPI_MR_READ`:
/// 4-byte address, octal DDR data, `PSRAM_MR_READ_DCYC` dummy cycles.
/// Returns the low byte of the response (the spec says only low byte
/// is meaningful for MRR).
fn psram_mr_read(regs: MpiPeri, mr_addr: u32) -> u8 {
    regs.dlr1().write(|w| w.set_dlen(1)); // 2 bytes
    regs.ccr1().write(|w| {
        w.set_imode(MODE_OCTAL_DDR);
        w.set_admode(MODE_OCTAL_DDR);
        w.set_adsize(3);
        w.set_abmode(0);
        w.set_dmode(MODE_OCTAL_DDR);
        w.set_dcyc(PSRAM_MR_READ_DCYC);
        w.set_fmode(false); // read mode
    });
    regs.ar1().write(|w| w.0 = mr_addr);
    regs.cmdr1().write(|w| w.set_cmd(CMD_PSRAM_MR_READ));
    wait_psram_tcf(regs);
    (regs.dr().read().0 & 0xff) as u8
}

/// Read a register, log the value, and verify it matches `expected`.
/// Used to confirm writes actually took on the chip side. Doesn't panic
/// on mismatch — bringing the system up far enough to log a useful diff
/// is more valuable than crashing here.
///
/// Note: rcard_log only supports `{}` placeholders; format specs like
/// `{:02x}` print literally. Values render as decimal — adequate for
/// "did this match?" diagnostics.
fn validate_mr(regs: MpiPeri, mr_addr: u32, expected: u8, name: &str) {
    let got = psram_mr_read(regs, mr_addr);
    if got == expected {
        info!("PSRAM {}: {} (ok)", name, got);
    } else {
        error!(
            "PSRAM {} mismatch: wrote {}, read {}",
            name, expected, got
        );
    }
}

/// Pre-init the SiP PSRAM's clock + power. Mirrors what the SDK's BSP
/// (e.g. `customer/boards/sf32lb52-lcd_base/bsp_init.c`) does *before*
/// it calls into `bsp_psramc_init`:
///
///   HAL_RCC_HCPU_EnableDLL2(240_000_000)
///   HAL_RCC_HCPU_ClockSelect(RCC_CLK_MOD_FLASH1, RCC_CLK_FLASH_DLL2)
///   HAL_PMU_ConfigPeriLdo(PMU_PERI_LDO_1V8, true, true)
///
/// Our kernel only locks DLL1 (firmware/kernels/sf32lb52/src/main.rs:266-289),
/// so DLL2 is dead at our entry point — we have to bring it up ourselves
/// before pointing MPI1 at it. Without DLL2 every MPI transaction
/// TCF-times-out (no clock to drive the bus). Without the LDO the PSRAM
/// die has no 1.8 V supply and can't respond regardless.
fn psram_pre_init() {
    let rcc = sifli_pac::HPSYS_RCC;
    let pmu = sifli_pac::PMUC;

    // 1. Bring DLL2 up at 240 MHz. SDK's HAL_RCC_HCPU_EnableDLL flow
    // (drivers/hal/bf0_hal_rcc.c:1632) also sets HPSYS_CFG.CAU2_CR
    // HPBG_EN + HPBG_VDDPSW_EN, but the kernel already does that for
    // DLL1 (firmware/kernels/sf32lb52/src/main.rs:222-224) and the
    // band-gap is shared across DLLs, so we don't need to touch it
    // here — and skipping it lets us avoid declaring `syscon` in the
    // task's `uses_peripherals`, which would push the MPU region count
    // over its 8-slot limit.
    //
    //   - clear DLL2CR.EN, set IN_DIV2_EN=1, OUT_DIV2_EN=0
    //   - STG = (freq - DLL_MIN_FREQ) / DLL_STG_STEP
    //         = (240e6 - 24e6) / 24e6 = 9
    //   - set EN=1, wait, poll READY
    rcc.dllcr(1).modify(|w| w.set_en(false));
    rcc.dllcr(1).modify(|w| {
        w.set_in_div2_en(true);
        w.set_out_div2_en(false);
        w.set_stg(9); // 240 MHz
        w.set_en(true);
    });
    spin_us(10);
    while !rcc.dllcr(1).read().ready() {
        core::hint::spin_loop();
    }

    // 2. MPI1 functional clock = DLL2. CSR.SEL_MPI1 lives at bits [5:4];
    // value 2 selects clk_dll2 (per the PAC + SDK's RCC_CLK_FLASH_DLL2).
    rcc.csr().modify(|w| w.set_sel_mpi1(2));

    // 3. Enable the 1.8 V peripheral LDO. PMUC.PERI_LDO bit 0 = enable
    // LDO18; bit 5 = power-down (must be cleared).
    pmu.peri_ldo().modify(|w| {
        w.0 = (w.0 & !((1 << 0) | (1 << 5))) | (1 << 0);
    });

    // 4. LDO settling — SDK's HAL_PMU_ConfigPeriLdo waits 5 ms.
    spin_us(5_000);
}

/// Bring up MPI1 + the SiP PSRAM for memory-mapped XIP access.
///
/// Mirrors the SiFli SDK's `bsp_psramc_init` → `HAL_MPI_PSRAM_Init` →
/// `HAL_OPI_PSRAM_Init` (drivers/hal/bf0_hal_mpi_psram.c) for the
/// `SPI_MODE_OPSRAM` + `freq <= 24 MHz` path. Constants are picked for
/// `PSCLR.div = 8` so we land squarely in that branch.
fn init_psram() {
    let regs = sifli_pac::MPI1;
    let rcc = sifli_pac::HPSYS_RCC;

    // 1. RCC block reset of MPI1. Same dance as the flash open() path:
    // BOOTROM may have left the controller mid-transaction with arbitrary
    // FIFO / IRQ state; a block reset is the only clean recovery.
    rcc.rstr2().modify(|w| w.set_mpi1(true));
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
    rcc.rstr2().modify(|w| w.set_mpi1(false));
    rcc.enr2().modify(|w| w.set_mpi1(true));

    // 2. HAL_QSPI_Init equivalent — base controller defaults the SDK sets
    // on every MPI bring-up.
    regs.timr().write(|w| w.0 = 0xFF);
    regs.cir().write(|w| w.0 = 0x5000_5000);
    regs.abr1().write(|w| w.0 = 0xFF);
    regs.hrabr().write(|w| w.0 = 0xFF);

    // 3. PSCLR — div=8 → low-clock branch (≤ 24 MHz PSRAM clock).
    // SF32LB52X DLL1 source is ~240 MHz so MCLK = 30 MHz, PSRAM clock = 15 MHz.
    // We don't have HAL_MPI_OPSRAM_CAL_DELAY (auto-calibration), so we
    // need to be in the low-clock branch where the SDK's explicit
    // dqs_dly=20 / sck_dly=20 values apply. Higher-clock branches expect
    // calibrated values.
    regs.psclr().write(|w| w.set_div(8));

    // 4. MISCR — DQS delay = 20, SCK delay = 20 (SF32LB52X-specific
    // low-clock values from `HAL_OPI_PSRAM_Init`).
    regs.miscr().write(|w| {
        w.set_sckdly(20);
        w.set_dqsdly(20);
    });

    // 5. DCR — HAL_FLASH_SET_CS_TIME + SET_ROW_BOUNDARY + ENABLE_DQS +
    // SET_FIXLAT (the EN_FIXLAT side; MR0/MR4 writes happen later).
    // SDK low-clock values:
    regs.dcr().write(|w| {
        w.set_rbsize(7); // 1024-byte rows; controller auto-handles refresh
        w.set_dqse(true); // required for octal DDR Rx latching
        w.set_hyper(false);
        w.set_xlegacy(false);
        w.set_cslmax(180); // ~12 µs at 15 MHz PSRAM clock
        w.set_cslmin(6);
        w.set_cshmin(0);
        w.set_trcmin(3);
        w.set_fixlat(true);
    });

    // 6. Bring controller online with OPI enabled.
    regs.cr().write(|w| {
        w.set_en(true);
        w.0 |= CR_OPIE;
    });

    // 7. PSRAM RESET — single octal-mode 0xFF, matching the SDK's
    // `HAL_PSRAM_RESET` for SPI_MODE_OPSRAM (LEGPSRAM resets twice; OPSRAM
    // does NOT). Issuing a quad-mode reset first — as I had previously —
    // leaves the controller in a state where the WDT triggers a bus
    // fault on the first AHB read.
    regs.ar1().write(|w| w.0 = 0);
    regs.ccr1().write(|w| {
        w.set_imode(MODE_OCTAL_DDR);
        w.set_admode(MODE_OCTAL_DDR);
        w.set_adsize(3); // 4-byte address
        w.set_abmode(MODE_OCTAL_DDR);
        w.set_absize(1); // 2-byte alt phase
        w.set_dmode(0);
        w.set_dcyc(0);
        w.set_fmode(true);
    });
    regs.cmdr1().write(|w| w.set_cmd(CMD_PSRAM_RESET));
    wait_psram_tcf(regs);
    spin_us(10); // SDK calls HAL_Delay_us(0) then HAL_Delay_us(3); pad to 10

    // 7b. Sanity probe — read MR1 (vendor ID) to confirm the chip
    // decoded our reset and is responding on the expected protocol. If
    // this comes back as 0x00 or 0xFF, the OPI/octal/DQS handshake is
    // wrong and nothing downstream will work.
    let vendor_id = psram_mr_read(regs, 1);
    info!("PSRAM vendor ID (MR1): {}", vendor_id);
    let device_id = psram_mr_read(regs, 2);
    info!("PSRAM device ID (MR2): {}", device_id);

    // 8. MR_WRITE(mr=8, value=3) — set drive strength.
    psram_mr_write(regs, 8, 3);
    validate_mr(regs, 8, 3, "MR8 (drive strength)");

    // 9. HCMDR — XIP read/write opcodes the controller emits per AHB
    // transfer.
    regs.hcmdr().write(|w| {
        w.set_rcmd(CMD_PSRAM_SYNC_READ);
        w.set_wcmd(CMD_PSRAM_SYNC_WRITE);
    });

    // 10. HRCCR — XIP read protocol. 4-byte address; dummy = r_lat - 1.
    regs.hrccr().write(|w| {
        w.set_imode(MODE_OCTAL_DDR);
        w.set_admode(MODE_OCTAL_DDR);
        w.set_adsize(3);
        w.set_abmode(0);
        w.set_absize(0);
        w.set_dcyc(PSRAM_R_LAT - 1);
        w.set_dmode(MODE_OCTAL_DDR);
    });

    // 11. HWCCR — XIP write protocol. Same as HRCCR with dummy = w_lat - 1.
    regs.hwccr().write(|w| {
        w.set_imode(MODE_OCTAL_DDR);
        w.set_admode(MODE_OCTAL_DDR);
        w.set_adsize(3);
        w.set_abmode(0);
        w.set_absize(0);
        w.set_dcyc(PSRAM_W_LAT - 1);
        w.set_dmode(MODE_OCTAL_DDR);
    });

    // 12. Configure the PSRAM's own latency mode to match. SDK's
    // HAL_MPI_SET_FIXLAT writes MR0 (read latency code) and MR4 (write
    // latency code). Without these the chip's internal latency setting
    // doesn't match the controller's HRCCR/HWCCR.dcyc values, and reads
    // come back shifted/garbled.
    psram_mr_write(regs, 0, PSRAM_MR0_FIXLAT);
    validate_mr(regs, 0, PSRAM_MR0_FIXLAT, "MR0 (read latency)");
    psram_mr_write(regs, 4, PSRAM_MR4_FIXLAT);
    validate_mr(regs, 4, PSRAM_MR4_FIXLAT, "MR4 (write latency)");

    // 13. Watchdog — AHB-side timeout so an unresponsive memory transfer
    // returns a bus error to the caller instead of hanging the bus
    // indefinitely.
    regs.wdtr().write(|w| {
        w.set_timeout(0xFFFF);
        w.set_en(true);
    });
}

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    // 1. Pre-init: clock source + LDO. PSRAM has no functional clock
    // and no 1.8 V supply otherwise — every MPI transaction TCF-times-
    // out and the chip can't respond.
    psram_pre_init();

    // 2. Controller + chip bring-up.
    init_psram();
    info!("PSRAM ready");

    ipc::server! {
        Mpi: MpiResource,
    }
}
