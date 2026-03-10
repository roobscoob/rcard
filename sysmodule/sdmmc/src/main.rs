#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use sf32lb52_pac::sdmmc1::RegisterBlock;
use sf32lb52_pac::Sdmmc1;

use hubris_task_slots::SLOTS;
use sysmodule_sdmmc_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
sysmodule_log_api::panic_handler!(Log);

static SDMMC_OPEN: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum SdmmcError {
    Timeout = 1,
    CrcError = 2,
    CardNotReady = 3,
}

fn regs() -> &'static RegisterBlock {
    unsafe { &*Sdmmc1::PTR }
}

// ── Low-level SD command helpers ────────────────────────────────────

/// Send an SD command and wait for completion.
/// Returns (rsp_index, rar1) for short responses.
fn send_cmd(cmd: u8, arg: u32, has_rsp: bool, long_rsp: bool) -> Result<(u32, u32), SdmmcError> {
    let r = regs();

    // Clear W1C status bits
    r.sr().write(|w| unsafe {
        w.bits(
            (1 << 1)   // cmd_done
            | (1 << 2) // cmd_rsp_crc
            | (1 << 3) // cmd_timeout
            | (1 << 5) // data_done
            | (1 << 12), // cmd_sent
        )
    });

    // Set argument
    r.car().write(|w| unsafe { w.bits(arg) });

    // Build CCR: cmd_index[23:18] | cmd_long_rsp[17] | cmd_has_rsp[16] | cmd_start[0]
    let mut ccr: u32 = (cmd as u32 & 0x3F) << 18;
    if has_rsp {
        ccr |= 1 << 16;
    }
    if long_rsp {
        ccr |= 1 << 17;
    }
    ccr |= 1; // cmd_start
    r.ccr().write(|w| unsafe { w.bits(ccr) });

    // Poll for cmd_done
    while !r.sr().read().cmd_done().bit_is_set() {}

    // Check for timeout and CRC errors in the status register
    let sr = r.sr().read();
    if sr.cmd_timeout().bit_is_set() {
        return Err(SdmmcError::Timeout);
    }
    if sr.cmd_rsp_crc().bit_is_set() {
        return Err(SdmmcError::CrcError);
    }

    let rsp_idx = r.rir().read().bits() & 0x3F;
    let rar1 = r.rar1().read().bits();
    Ok((rsp_idx, rar1))
}

/// Send CMD55 (APP_CMD) followed by an application command.
fn send_acmd(rca: u16, cmd: u8, arg: u32, has_rsp: bool) -> Result<(u32, u32), SdmmcError> {
    send_cmd(55, (rca as u32) << 16, true, false)?;
    send_cmd(cmd, arg, has_rsp, false)
}

// ── SD card initialization ──────────────────────────────────────────

struct CardInfo {
    block_count: u32,
}

fn init_card() -> Result<CardInfo, SdmmcOpenError> {
    let r = regs();

    // Enable card detect
    r.cdr().write(|w| unsafe { w.bits(0x19) });

    // Check card presence
    if !r.sr().read().card_exist().bit_is_set() {
        return Err(SdmmcOpenError::InitFailed);
    }

    // Set clock to ~400kHz for identification (240MHz / (599+1) = 400kHz)
    r.clkcr()
        .write(|w| unsafe { w.div().bits(599).stop_clk().clear_bit() });

    // Set block size to 512, 1-bit bus initially
    r.dcr()
        .write(|w| unsafe { w.block_size().bits(0x1FF).wire_mode().bits(0) });

    // CMD0: GO_IDLE_STATE (no response)
    let _ = send_cmd(0, 0, false, false);

    // CMD8: SEND_IF_COND — voltage check (pattern 0xAA)
    let (_idx, r1) = send_cmd(8, 0x1AA, true, false)
        .map_err(|_| SdmmcOpenError::InitFailed)?;
    if (r1 & 0xFFF) != 0x1AA {
        return Err(SdmmcOpenError::InitFailed);
    }

    // ACMD41: SD_SEND_OP_COND — wait for card ready (HCS=1 for SDHC)
    // TODO: SD spec recommends >= 1ms delay between ACMD41 retries.
    // Current implementation polls without delay, which may fail on slow cards.
    let mut tries = 0u32;
    loop {
        let (_idx, ocr) = send_acmd(0, 41, 0x40FF_8000, true)
            .map_err(|_| SdmmcOpenError::InitFailed)?;
        if ocr & (1 << 31) != 0 {
            break;
        }
        tries += 1;
        if tries > 1000 {
            return Err(SdmmcOpenError::InitFailed);
        }
    }

    // CMD2: ALL_SEND_CID (long response)
    send_cmd(2, 0, true, true)
        .map_err(|_| SdmmcOpenError::InitFailed)?;

    // CMD3: SEND_RELATIVE_ADDR — get RCA
    let (_idx, r6) = send_cmd(3, 0, true, false)
        .map_err(|_| SdmmcOpenError::InitFailed)?;
    let rca = (r6 >> 16) as u16;

    // CMD9: SEND_CSD — read card capacity (long response)
    send_cmd(9, (rca as u32) << 16, true, true)
        .map_err(|_| SdmmcOpenError::InitFailed)?;
    let rar2 = regs().rar2().read().bits();
    // SDHC CSD v2: C_SIZE in RAR2[29:8] (22 bits)
    let c_size = (rar2 >> 8) & 0x3F_FFFF;
    let block_count = (c_size + 1) * 1024;

    // CMD7: SELECT_CARD
    send_cmd(7, (rca as u32) << 16, true, false)
        .map_err(|_| SdmmcOpenError::InitFailed)?;

    // ACMD6: SET_BUS_WIDTH to 4-bit
    send_acmd(rca, 6, 2, true)
        .map_err(|_| SdmmcOpenError::InitFailed)?;

    // Switch DCR to 4-wire mode
    r.dcr().modify(|_, w| unsafe { w.wire_mode().bits(1) });

    // Switch to fast clock: 240MHz / (4+1) = 48MHz
    r.clkcr()
        .write(|w| unsafe { w.div().bits(4).stop_clk().clear_bit() });

    Ok(CardInfo { block_count })
}

// ── Block read/write ────────────────────────────────────────────────

fn read_block_hw(block_addr: u32, buf: &mut [u8; 512]) -> Result<(), SdmmcError> {
    let r = regs();

    // Data length = 512 bytes
    r.dlr().write(|w| unsafe { w.data_len().bits(0x1FF) });

    // Configure read: tran_data_en, r_wn=read, block mode, 4-wire, start
    r.dcr().write(|w| unsafe {
        w.block_size()
            .bits(0x1FF)
            .wire_mode()
            .bits(1)
            .r_wn()
            .set_bit()
            .tran_data_en()
            .set_bit()
            .data_start()
            .set_bit()
    });

    // CMD17: READ_SINGLE_BLOCK (SDHC uses block addressing)
    send_cmd(17, block_addr, true, false)?;

    // Read 128 words from FIFO
    for i in (0..512).step_by(4) {
        let word = r.fifo().read().bits();
        buf[i] = word as u8;
        buf[i + 1] = (word >> 8) as u8;
        buf[i + 2] = (word >> 16) as u8;
        buf[i + 3] = (word >> 24) as u8;
    }

    // Wait for data_done
    while !r.sr().read().data_done().bit_is_set() {}
    r.sr().write(|w| unsafe { w.bits(1 << 5) });
    Ok(())
}

fn write_block_hw(block_addr: u32, buf: &[u8; 512]) -> Result<(), SdmmcError> {
    let r = regs();

    // Data length = 512 bytes
    r.dlr().write(|w| unsafe { w.data_len().bits(0x1FF) });

    // Configure write: tran_data_en, r_wn=write, block mode, 4-wire, start
    r.dcr().write(|w| unsafe {
        w.block_size()
            .bits(0x1FF)
            .wire_mode()
            .bits(1)
            .r_wn()
            .clear_bit()
            .tran_data_en()
            .set_bit()
            .data_start()
            .set_bit()
    });

    // CMD24: WRITE_BLOCK
    send_cmd(24, block_addr, true, false)?;

    // Write 128 words to FIFO
    for i in (0..512).step_by(4) {
        let word = buf[i] as u32
            | (buf[i + 1] as u32) << 8
            | (buf[i + 2] as u32) << 16
            | (buf[i + 3] as u32) << 24;
        r.fifo().write(|w| unsafe { w.bits(word) });
    }

    // Wait for data_done
    while !r.sr().read().data_done().bit_is_set() {}
    r.sr().write(|w| unsafe { w.bits(1 << 5) });
    Ok(())
}

// ── IPC resource implementation ─────────────────────────────────────

struct SdmmcResource {
    block_count: u32,
}

impl Sdmmc for SdmmcResource {
    fn open(meta: ipc::Meta) -> Result<Self, SdmmcOpenError> {
        log::trace!("Task {:?} attempting acquire", meta.sender);

        if SDMMC_OPEN.swap(true, Ordering::Acquire) {
            log::error!("Task {:?} failed to acquire (already in use)", meta.sender);

            return Err(SdmmcOpenError::ReservedSlot);
        }

        match init_card() {
            Ok(info) => Ok(SdmmcResource {
                block_count: info.block_count,
            }),
            Err(e) => {
                log::error!("Task {:?} failed to initialize: {:?}", meta.sender, e);
                SDMMC_OPEN.store(false, Ordering::Release);
                Err(e)
            }
        }
    }

    fn read_block(
        &mut self,
        _meta: ipc::Meta,
        block: u32,
        buf: idyll_runtime::Leased<idyll_runtime::Write, u8>,
    ) -> Result<(), BlockError> {
        log::trace!("read_block block={} len={}", block, buf.len());

        if block >= self.block_count {
            return Err(BlockError::OutOfRange);
        }

        // Caller must provide a full block buffer.
        if buf.len() < 512 {
            return Err(BlockError::Device(0xFFFF));
        }

        let mut tmp = [0u8; 512];
        match read_block_hw(block, &mut tmp) {
            Ok(()) => {
                for i in 0..512 {
                    let _ = buf.write(i, tmp[i]);
                }
                Ok(())
            }
            Err(e) => {
                log::warn!("sdmmc read_block failed: {:?}", e);
                Err(BlockError::Device(e as u16))
            }
        }
    }

    fn write_block(
        &mut self,
        _meta: ipc::Meta,
        block: u32,
        buf: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<(), BlockError> {
        log::trace!("write_block block={} len={}", block, buf.len());

        if block >= self.block_count {
            return Err(BlockError::OutOfRange);
        }

        // Caller must provide a full block buffer.
        if buf.len() < 512 {
            return Err(BlockError::Device(0xFFFF));
        }

        let mut tmp = [0u8; 512];
        for i in 0..512 {
            tmp[i] = buf.read(i).unwrap_or(0);
        }
        match write_block_hw(block, &tmp) {
            Ok(()) => Ok(()),
            Err(e) => {
                log::warn!("sdmmc write_block failed: {:?}", e);
                Err(BlockError::Device(e as u16))
            }
        }
    }

    fn block_count(&mut self, _meta: ipc::Meta) -> u32 {
        self.block_count
    }
}

impl Drop for SdmmcResource {
    fn drop(&mut self) {
        log::trace!("Released");
        SDMMC_OPEN.store(false, Ordering::Release);
    }
}

#[export_name = "main"]
fn main() -> ! {
    sysmodule_log_api::init_logger!(Log);

    ipc::server! {
        Sdmmc: SdmmcResource,
    }
}
