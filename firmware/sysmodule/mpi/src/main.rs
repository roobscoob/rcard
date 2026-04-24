#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use generated::slots::SLOTS;
use rcard_log::{debug, error, info, panic, trace, OptionExt};
use sifli_pac::mpi::Mpi as MpiPeri;
use sysmodule_mpi_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);

static MPI_IN_USE: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

fn mpi_instance(index: u8) -> Option<MpiPeri> {
    match index {
        1 => Some(sifli_pac::MPI1),
        2 => Some(sifli_pac::MPI2),
        _ => None,
    }
}

// Standard SPI NOR flash commands
const CMD_WRITE_ENABLE: u8 = 0x06;
const CMD_READ_STATUS_1: u8 = 0x05;
const CMD_READ_JEDEC_ID: u8 = 0x9F;
// 256 Mb chip (32 MB) needs 4-byte addressing to reach the upper half;
// we put it in 4-byte mode at open and use the 4-byte opcode variants
// for every address-bearing command. The 3-byte counterparts (in
// parens) are silently ignored by the chip when in 4-byte mode because
// CS# rises one byte short of the "last address byte" requirement that
// SE/BE/PP all share.
const CMD_RESET_ENABLE: u8 = 0x66; // RST_EN — arms the chip for reset
const CMD_RESET: u8 = 0x99; // RST — performs the soft reset
const CMD_RELEASE_DPD: u8 = 0xAB; // RDPD — wakes chip from Deep Power-Down
                                  // const CMD_ENTER_4BYTE_MODE: u8 = 0xB7; // EN4B — ADS bit goes 0 → 1
const CMD_READ_DATA: u8 = 0x13; // 4READ (was RD 03H)
const CMD_FAST_READ: u8 = 0x0C; // 4FR  (was FR 0BH)
const CMD_PAGE_PROGRAM: u8 = 0x12; // 4PP  (was PP 02H)
const CMD_SECTOR_ERASE_4K: u8 = 0x21; // 4SE  (was SE 20H)
const CMD_BLOCK_ERASE_32K: u8 = 0x5C; // 4BE32 (was BE32 52H)
const CMD_BLOCK_ERASE_64K: u8 = 0xDC; // 4BE64 (was BE64 D8H)
const CMD_CHIP_ERASE: u8 = 0xC7; // CE — no address, unchanged

const PAGE_SIZE: u32 = 256;

// Poll limits. Sized for worst-case at minimum supported clock.
// Transfer complete / FIFO operations should resolve in microseconds.
const MAX_TRANSFER_POLLS: u32 = 1_000_000;
// FIFO ready should resolve quickly per word.
const MAX_FIFO_POLLS: u32 = 100_000;
// WIP polling: chip erase can take tens of seconds at slow clocks.
// Each WIP poll is itself a full SPI transaction, so this counts
// outer iterations, not raw register reads.
const MAX_WIP_POLLS: u32 = 1_000_000;

#[derive(Debug, rcard_log::Format)]
enum HwTimeout {
    Transfer,
    RxFifo,
    TxFifo,
    Wip,
}

struct MpiResource {
    index: u8,
    regs: MpiPeri,
    config: MpiConfig,
}

#[allow(clippy::double_must_use)]
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

    /// Send a command-only sequence (no address, no data).
    fn cmd_only(&self, instruction: u8) -> Result<(), HwTimeout> {
        self.cmd_only_imode(instruction, self.config.imode)
    }

    /// Send a command-only sequence with an explicit instruction-line mode.
    /// Used during open() to send reset/wake on quad lines (covering BOOTROM
    /// having left the chip in QPI mode) before re-trying on single lines.
    fn cmd_only_imode(&self, instruction: u8, imode: LineMode) -> Result<(), HwTimeout> {
        self.regs.ccr1().write(|w| {
            w.set_imode(imode as u8);
        });
        // CMDR1 write triggers the hardware sequence — must come after CCR1
        self.regs.cmdr1().write(|w| w.set_cmd(instruction));
        self.wait_transfer_complete()
    }

    /// Send a command + address, no data.
    fn cmd_addr(&self, instruction: u8, address: u32) -> Result<(), HwTimeout> {
        self.regs.ar1().write(|w| w.0 = address);
        self.regs.ccr1().write(|w| {
            w.set_imode(self.config.imode as u8);
            w.set_admode(self.config.admode as u8);
            w.set_adsize(self.config.addr_size as u8);
        });
        // CMDR1 write triggers the hardware sequence — must come after CCR1/AR1
        self.regs.cmdr1().write(|w| w.set_cmd(instruction));
        self.wait_transfer_complete()
    }

    fn write_enable(&self) -> Result<(), HwTimeout> {
        self.cmd_only(CMD_WRITE_ENABLE)
    }

    /// Poll status register 1 until WIP (bit 0) clears.
    fn wait_wip(&self) -> Result<(), HwTimeout> {
        for n in 0..MAX_WIP_POLLS {
            self.regs.dlr1().write(|w| w.0 = 0); // 1 byte (n-1 encoding)
            self.regs.ccr1().write(|w| {
                w.set_imode(self.config.imode as u8);
                w.set_dmode(self.config.dmode as u8);
            });
            // CMDR1 write triggers the hardware sequence — must come after CCR1/DLR1
            self.regs.cmdr1().write(|w| w.set_cmd(CMD_READ_STATUS_1));
            self.wait_transfer_complete()?;
            self.wait_rx_ready()?;
            let sr = self.regs.dr().read().0 as u8;
            if sr & 0x01 == 0 {
                trace!("MPI{}: WIP cleared after {} polls", self.index, n);
                return Ok(());
            }
        }
        Err(HwTimeout::Wip)
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
        };

        loop {
            // Full cycle: LDO off → LDO on → RCC reset MPI peripheral →
            // reconfigure MPI regs → chip soft reset → JEDEC.

            // 1. Drop flash VCC.
            // sifli_pac::PMUC
            //     .peri_ldo()
            //     .modify(|peri_ldo| peri_ldo.set_en_vdd33_ldo3(false));

            // info!("MPI{}: flash power cut, waiting for stabilization", index);

            // for _ in 0..10_000_000 {
            //     core::hint::spin_loop();
            // }

            // info!("MPI{}: flash power down complete", index);

            // // 2. Bring flash VCC back up.
            // sifli_pac::PMUC
            //     .peri_ldo()
            //     .modify(|peri_ldo| peri_ldo.set_en_vdd33_ldo3(true));

            // info!(
            //     "MPI{}: flash power restored, waiting for stabilization",
            //     index
            // );

            // for _ in 0..10_000_000 {
            //     core::hint::spin_loop();
            // }

            // info!("MPI{}: flash power up complete", index);

            // 3. Hard-reset the MPI2 peripheral via HPSYS_RCC. Clears any
            //    internal state machine / FIFO / CS# activity left over
            //    from the BOOTROM's XIP bring-up — that state survives
            //    MCU warm resets and was leaking into this session as
            //    "JEDEC returns 0,0,0 deterministically within a boot."
            //    Mirrors the pattern in sysmodule_usb's bring-up.
            let rcc = sifli_pac::HPSYS_RCC;
            match index {
                1 => {
                    rcc.rstr2().modify(|w| w.set_mpi1(true));
                    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);

                    info!("MPI{}: RCC reset, waiting for stabilization", index);

                    for _ in 0..10_000_000 {
                        core::hint::spin_loop();
                    }
                    rcc.rstr2().modify(|w| w.set_mpi1(false));
                    rcc.enr2().modify(|w| w.set_mpi1(true));
                }
                2 => {
                    rcc.rstr2().modify(|w| w.set_mpi2(true));
                    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);

                    info!("MPI{}: RCC reset, waiting for stabilization", index);

                    for _ in 0..10_000_000 {
                        core::hint::spin_loop();
                    }
                    rcc.rstr2().modify(|w| w.set_mpi2(false));
                    rcc.enr2().modify(|w| w.set_mpi2(true));
                }
                _ => unreachable!("index validated above"),
            }

            // 4. Reconfigure the (now clean) MPI peripheral. RCC reset
            //    zeroed every register so we re-apply.
            //
            //    The TIMR/CIR/ABR1/HRABR writes mirror the SDK's
            //    `HAL_QSPI_Init` (drivers/hal/bf0_hal_qspi.c). CIR in
            //    particular is the "command interval" — minimum gap
            //    between consecutive commands. Default 0 = back-to-back,
            //    which appears to confuse the chip during init when it's
            //    still settling from a wake-from-DP. SDK uses 0x5000 in
            //    both halves (~85 µs at 240 MHz).
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

            info!(
                "MPI{}: peripheral reset and reconfiguration complete",
                index
            );

            // 5. Wake + reset on QUAD lines first. If BOOTROM left the
            //    chip in QPI mode (entered via 0x38 for fast XIP), single-
            //    line commands look like garbled bits on DIO0 to the chip
            //    and are ignored — including RDPD and RST. So we send
            //    RDPD/RST_EN/RST on 4 lanes first, which the QPI chip
            //    receives as proper commands. A non-QPI chip sees these as
            //    fragmentary garbage (4-line byte = 2 clocks vs 8 expected)
            //    and aborts mid-instruction with no side effect — safe to
            //    send blind. After this, chip should be in default SPI mode
            //    regardless of which one it started in.
            for &cmd in &[CMD_RELEASE_DPD, CMD_RESET_ENABLE, CMD_RESET] {
                let _ = resource.cmd_only_imode(cmd, LineMode::Quad);

                info!(
                    "MPI{}: sent {} on quad lines, waiting for stabilization",
                    index, cmd
                );

                for _ in 0..10_000_000 {
                    core::hint::spin_loop();
                }
            }

            // 6. Wake + reset on SINGLE lines. Either the chip was already
            //    in SPI mode (QPI attempt above was a no-op), or the QPI
            //    reset above just put it in SPI. Either way it should now
            //    accept single-line commands.
            //    RDPD: tRES1 ≥ 50 µs — 60k cycles ≈ 750 µs at 240 MHz.
            //    Between RST_EN and RST: SDK uses 300 µs (see
            //    HAL_QSPIEX_FLASH_RESET in bf0_hal_mpi_ex.c); without it,
            //    RST is silently dropped.
            if let Err(e) = resource.cmd_only(CMD_RELEASE_DPD) {
                error!("MPI{}: RDPD failed: {}", index, e);
            }

            info!(
                "MPI{}: sent RDPD on single lines, waiting for stabilization",
                index
            );

            for _ in 0..10_000_000 {
                core::hint::spin_loop();
            }

            if let Err(e) = resource.cmd_only(CMD_RESET_ENABLE) {
                error!("MPI{}: RST_EN failed: {}", index, e);
            }

            info!(
                "MPI{}: sent RST_EN on single lines, waiting for stabilization",
                index
            );

            for _ in 0..10_000_000 {
                core::hint::spin_loop();
            }

            if let Err(e) = resource.cmd_only(CMD_RESET) {
                error!("MPI{}: RST failed: {}", index, e);
            }

            info!(
                "MPI{}: sent RST on single lines, waiting for stabilization",
                index
            );

            for _ in 0..10_000_000 {
                core::hint::spin_loop();
            }

            debug!(
                "MPI{}: opened, format={}",
                index,
                resource.read_jedec_id(meta)
            );
        }

        // Force 4-byte address mode. The GD25Q256E (256 Mb / 32 MB) needs
        // 4-byte addressing to reach the upper half, and our CMD_*
        // opcodes are the 4-byte variants (4SE/4PP/4READ/etc.). EN4B is
        // command-only, no address, no data — idempotent if the chip is
        // already in 4-byte mode (e.g. set by the BOOTROM or by ADP=1).
        // if let Err(e) = resource.cmd_only(CMD_ENTER_4BYTE_MODE) {
        //     error!("MPI{}: EN4B failed: {}", index, e);
        // }

        // Ok(resource)
    }

    fn read_jedec_id(&mut self, _meta: ipc::Meta) -> JedecId {
        self.regs.dlr1().write(|w| w.0 = 2); // 3 bytes (n-1 encoding)
        self.regs.ccr1().write(|w| {
            w.set_imode(self.config.imode as u8);
            w.set_dmode(self.config.dmode as u8);
        });
        // CMDR1 write triggers the hardware sequence — must come after CCR1/DLR1
        self.regs.cmdr1().write(|w| w.set_cmd(CMD_READ_JEDEC_ID));
        if let Err(e) = self.wait_transfer_complete() {
            panic!(
                "Timeout waiting for transfer complete during read_jedec_id: {}",
                e
            );
        }

        if let Err(e) = self.wait_rx_ready() {
            panic!("Timeout waiting for RX ready during read_jedec_id: {}", e);
        }

        let raw = self.regs.dr().read().0;

        JedecId {
            manufacturer: raw as u8,
            memory_type: (raw >> 8) as u8,
            capacity: (raw >> 16) as u8,
        }
    }

    fn read(
        &mut self,
        _meta: ipc::Meta,
        address: u32,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) {
        let len = buf.len();
        if len == 0 {
            return;
        }
        self.regs.dlr1().write(|w| w.0 = (len - 1) as u32);
        self.regs.ar1().write(|w| w.0 = address);
        self.regs.ccr1().write(|w| {
            w.set_imode(self.config.imode as u8);
            w.set_admode(self.config.admode as u8);
            w.set_adsize(self.config.addr_size as u8);
            w.set_dmode(self.config.dmode as u8);
            w.set_dcyc(self.config.read_dummy_cycles);
        });
        let read_cmd = if self.config.read_dummy_cycles > 0 {
            CMD_FAST_READ
        } else {
            CMD_READ_DATA
        };
        // CMDR1 write triggers the hardware sequence — must come after CCR1/AR1/DLR1
        self.regs.cmdr1().write(|w| w.set_cmd(read_cmd));

        // Accumulate FIFO words into a local buffer, then bulk-write to
        // the lease in one syscall — matches the bulk pattern in `write`.
        // Caller chunks at <= 256 bytes (sysmodule_storage), so this fits.
        let mut local = [0u8; PAGE_SIZE as usize];
        if len > local.len() {
            panic!(
                "MPI read length {} exceeds local buffer {}",
                len,
                local.len()
            );
        }

        let mut i = 0;
        while i < len {
            if let Err(e) = self.wait_rx_ready() {
                panic!("Timeout waiting for RX ready during read: {}", e);
            }

            let word = self.regs.dr().read().0;
            for byte_idx in 0..4 {
                if i < len {
                    local[i] = (word >> (byte_idx * 8)) as u8;
                    i += 1;
                }
            }
        }

        if let Err(e) = self.wait_transfer_complete() {
            panic!("Timeout waiting for transfer complete during read: {}", e);
        }

        buf.write_range(0, &local[..len]).log_unwrap();
    }

    fn write(
        &mut self,
        _meta: ipc::Meta,
        address: u32,
        data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) {
        let total = data.len();
        if total == 0 {
            return;
        }

        let mut offset: usize = 0;
        let mut addr = address;

        // Local buffer holding one page's worth of bytes. We bulk-read
        // each chunk from the lease in a single syscall before pushing
        // word-by-word to the FIFO — avoids one syscall per byte (slow,
        // and historically corruption-prone, see sysmodule_usb).
        let mut local = [0u8; PAGE_SIZE as usize];

        while offset < total {
            // Bytes remaining in the current page
            let page_remaining = (PAGE_SIZE - (addr % PAGE_SIZE)) as usize;
            let chunk = core::cmp::min(total - offset, page_remaining);

            data.read_range(offset, &mut local[..chunk]).log_unwrap();

            if let Err(e) = self.write_enable() {
                panic!("Timeout enabling write during write: {}", e);
            }

            self.regs.dlr1().write(|w| w.0 = (chunk - 1) as u32);
            self.regs.ar1().write(|w| w.0 = addr);
            self.regs.ccr1().write(|w| {
                w.set_imode(self.config.imode as u8);
                w.set_admode(self.config.admode as u8);
                w.set_adsize(self.config.addr_size as u8);
                w.set_dmode(self.config.dmode as u8);
                w.set_fmode(true); // write mode
            });
            // CMDR1 write triggers the hardware sequence — must come after CCR1/AR1/DLR1
            self.regs.cmdr1().write(|w| w.set_cmd(CMD_PAGE_PROGRAM));

            let mut i = 0;
            while i < chunk {
                let mut word: u32 = 0;
                for byte_idx in 0..4 {
                    if i < chunk {
                        word |= (local[i] as u32) << (byte_idx * 8);
                        i += 1;
                    }
                }

                if let Err(e) = self.wait_tx_ready() {
                    panic!("Timeout waiting for TX ready during write: {}", e);
                }

                self.regs.dr().write(|w| w.0 = word);
            }

            if let Err(e) = self.wait_transfer_complete() {
                panic!("Timeout waiting for transfer complete during write: {}", e);
            }

            if let Err(e) = self.wait_wip() {
                panic!("Timeout waiting for WIP during write: {}", e);
            }

            offset += chunk;
            addr += chunk as u32;
        }
    }

    fn erase(&mut self, _meta: ipc::Meta, address: u32, length: u32) -> Result<(), EraseError> {
        debug!(
            "MPI{}: erase address={} length={}",
            self.index, address, length
        );
        const ALIGN_4K: u32 = 4096;
        const ALIGN_32K: u32 = 32 * 1024;
        const ALIGN_64K: u32 = 64 * 1024;

        if address % ALIGN_4K != 0 {
            return Err(EraseError::InvalidAddressAlignment);
        }
        if length % ALIGN_4K != 0 {
            return Err(EraseError::InvalidLengthAlignment);
        }

        let mut addr = address;
        let end = address + length;

        while addr < end {
            let remaining = end - addr;
            let (cmd, step, label) = if addr % ALIGN_64K == 0 && remaining >= ALIGN_64K {
                (CMD_BLOCK_ERASE_64K, ALIGN_64K, "64K")
            } else if addr % ALIGN_32K == 0 && remaining >= ALIGN_32K {
                (CMD_BLOCK_ERASE_32K, ALIGN_32K, "32K")
            } else {
                (CMD_SECTOR_ERASE_4K, ALIGN_4K, "4K")
            };
            trace!("MPI{}: erasing {} block at {}", self.index, label, addr);

            if let Err(e) = self.write_enable() {
                panic!("Timeout enabling write during erase: {}", e);
            }

            if let Err(e) = self.cmd_addr(cmd, addr) {
                panic!("Timeout sending command during erase: {}", e);
            }

            if let Err(e) = self.wait_wip() {
                panic!("Timeout waiting for WIP during erase: {}", e);
            }

            addr += step;
        }

        Ok(())
    }

    fn erase_chip(&mut self, _meta: ipc::Meta) {
        debug!("MPI{}: erase_chip", self.index);
        if let Err(e) = self.write_enable() {
            panic!("Timeout enabling write during chip erase: {}", e);
        }

        if let Err(e) = self.cmd_only(CMD_CHIP_ERASE) {
            panic!("Timeout sending chip erase command: {}", e);
        }

        if let Err(e) = self.wait_wip() {
            panic!("Timeout waiting for WIP during chip erase: {}", e);
        }
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

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    ipc::server! {
        Mpi: MpiResource,
    }
}
