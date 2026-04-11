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
const CMD_READ_DATA: u8 = 0x03;
const CMD_FAST_READ: u8 = 0x0B;
const CMD_READ_JEDEC_ID: u8 = 0x9F;
const CMD_PAGE_PROGRAM: u8 = 0x02;
const CMD_SECTOR_ERASE_4K: u8 = 0x20;
const CMD_BLOCK_ERASE_32K: u8 = 0x52;
const CMD_BLOCK_ERASE_64K: u8 = 0xD8;
const CMD_CHIP_ERASE: u8 = 0xC7;

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
        self.regs.ccr1().write(|w| {
            w.set_imode(self.config.imode as u8);
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
        for _ in 0..MAX_WIP_POLLS {
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

        regs.cr().write(|w| {
            w.set_en(true);
        });
        regs.psclr().write(|w| w.set_div(config.prescaler));
        regs.miscr().modify(|w| {
            w.set_sckinv(config.clock_polarity as u8 != 0);
        });

        let mut resource = MpiResource {
            index,
            regs,
            config,
        };

        debug!(
            "MPI{}: opened, format={}",
            index,
            resource.read_jedec_id(meta)
        );

        Ok(resource)
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

        let mut i = 0;
        while i < len {
            if let Err(e) = self.wait_rx_ready() {
                panic!("Timeout waiting for RX ready during read: {}", e);
            }

            let word = self.regs.dr().read().0;
            // Each DR read pops a 32-bit word; unpack up to 4 bytes
            for byte_idx in 0..4 {
                if i < len {
                    buf.write(i, (word >> (byte_idx * 8)) as u8).log_unwrap();
                    i += 1;
                }
            }
        }

        if let Err(e) = self.wait_transfer_complete() {
            panic!("Timeout waiting for transfer complete during read: {}", e);
        }
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

        while offset < total {
            // Bytes remaining in the current page
            let page_remaining = (PAGE_SIZE - (addr % PAGE_SIZE)) as usize;
            let chunk = core::cmp::min(total - offset, page_remaining);

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
                        let b = data.read(offset + i).log_unwrap();
                        word |= (b as u32) << (byte_idx * 8);
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
