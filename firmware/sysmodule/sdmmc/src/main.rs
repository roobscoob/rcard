#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use sifli_pac::sdmmc::Sdmmc as SdmmcPeri;

use hubris_task_slots::SLOTS;
use sysmodule_sdmmc_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);

static SDMMC_OPEN: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum SdmmcError {
    Timeout = 1,
    CrcError = 2,
    CardNotReady = 3,
}

fn regs() -> SdmmcPeri {
    sifli_pac::SDMMC1
}

// ── Low-level SD command helpers ────────────────────────────────────

/// Send an SD command and wait for completion.
/// Returns (rsp_index, rar1) for short responses.
fn send_cmd(cmd: u8, arg: u32, has_rsp: bool, long_rsp: bool) -> Result<(u32, u32), SdmmcError> {
    let r = regs();

    // Clear W1C status bits
    r.sr().write(|w| {
        w.set_cmd_done(true);
        w.set_cmd_rsp_crc(true);
        w.set_cmd_timeout(true);
        w.set_data_done(true);
        w.set_cmd_sent(true);
    });

    // Set argument
    r.car().write(|w| w.set_cmd_arg(arg));

    // Build and send command
    r.ccr().write(|w| {
        w.set_cmd_index(cmd);
        w.set_cmd_has_rsp(has_rsp);
        w.set_cmd_long_rsp(long_rsp);
        w.set_cmd_start(true);
    });

    // Poll for cmd_done
    while !r.sr().read().cmd_done() {}

    // Check for timeout and CRC errors in the status register
    let sr = r.sr().read();
    if sr.cmd_timeout() {
        return Err(SdmmcError::Timeout);
    }
    if sr.cmd_rsp_crc() {
        return Err(SdmmcError::CrcError);
    }

    let rsp_idx = r.rir().read().rsp_index() as u32;
    let rar1 = r.rar1().read().rsp_arg1();
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

#[cold]
fn init_card() -> Result<CardInfo, SdmmcOpenError> {
    let r = regs();

    // Enable card detect
    r.cdr().write(|w| {
        w.set_sd_data3_cd(true);
        w.set_en_cd(true);
        w.set_cd_hvalid(true);
    });

    // Check card presence
    if !r.sr().read().card_exist() {
        return Err(SdmmcOpenError::InitFailed);
    }

    // Set clock to ~400kHz for identification (240MHz / (599+1) = 400kHz)
    r.clkcr().write(|w| {
        w.set_div(599);
        w.set_stop_clk(false);
    });

    // Set block size to 512, 1-bit bus initially
    r.dcr().write(|w| {
        w.set_block_size(0x1FF);
        w.set_wire_mode(0);
    });

    // CMD0: GO_IDLE_STATE (no response)
    let _ = send_cmd(0, 0, false, false);

    // CMD8: SEND_IF_COND — voltage check (pattern 0xAA)
    let (_idx, r1) = send_cmd(8, 0x1AA, true, false).map_err(|_| SdmmcOpenError::InitFailed)?;
    if (r1 & 0xFFF) != 0x1AA {
        return Err(SdmmcOpenError::InitFailed);
    }

    // ACMD41: SD_SEND_OP_COND — wait for card ready (HCS=1 for SDHC)
    // TODO: SD spec recommends >= 1ms delay between ACMD41 retries.
    // Current implementation polls without delay, which may fail on slow cards.
    let mut tries = 0u32;
    loop {
        let (_idx, ocr) =
            send_acmd(0, 41, 0x40FF_8000, true).map_err(|_| SdmmcOpenError::InitFailed)?;
        if ocr & (1 << 31) != 0 {
            break;
        }
        tries += 1;
        if tries > 1000 {
            return Err(SdmmcOpenError::InitFailed);
        }
    }

    // CMD2: ALL_SEND_CID (long response)
    send_cmd(2, 0, true, true).map_err(|_| SdmmcOpenError::InitFailed)?;

    // CMD3: SEND_RELATIVE_ADDR — get RCA
    let (_idx, r6) = send_cmd(3, 0, true, false).map_err(|_| SdmmcOpenError::InitFailed)?;
    let rca = (r6 >> 16) as u16;

    // CMD9: SEND_CSD — read card capacity (long response)
    send_cmd(9, (rca as u32) << 16, true, true).map_err(|_| SdmmcOpenError::InitFailed)?;
    let rar2 = regs().rar2().read().rsp_arg2();
    // SDHC CSD v2: C_SIZE in RAR2[29:8] (22 bits)
    let c_size = (rar2 >> 8) & 0x3F_FFFF;
    let block_count = (c_size + 1) * 1024;

    // CMD7: SELECT_CARD
    send_cmd(7, (rca as u32) << 16, true, false).map_err(|_| SdmmcOpenError::InitFailed)?;

    // ACMD6: SET_BUS_WIDTH to 4-bit
    send_acmd(rca, 6, 2, true).map_err(|_| SdmmcOpenError::InitFailed)?;

    // Switch DCR to 4-wire mode
    r.dcr().modify(|w| w.set_wire_mode(1));

    // Switch to fast clock: 240MHz / (4+1) = 48MHz
    r.clkcr().write(|w| {
        w.set_div(4);
        w.set_stop_clk(false);
    });

    Ok(CardInfo { block_count })
}

// ── Block read/write ────────────────────────────────────────────────

fn read_block_hw(block_addr: u32, buf: &mut [u8; 512]) -> Result<(), SdmmcError> {
    let r = regs();

    // Data length = 512 bytes
    r.dlr().write(|w| w.set_data_len(0x1FF));

    // Configure read: tran_data_en, r_wn=read, block mode, 4-wire, start
    r.dcr().write(|w| {
        w.set_block_size(0x1FF);
        w.set_wire_mode(1);
        w.set_r_wn(true);
        w.set_tran_data_en(true);
        w.set_data_start(true);
    });

    // CMD17: READ_SINGLE_BLOCK (SDHC uses block addressing)
    send_cmd(17, block_addr, true, false)?;

    // Read 128 words from FIFO
    for i in (0..512).step_by(4) {
        let word = r.fifo().read().data();
        buf[i] = word as u8;
        buf[i + 1] = (word >> 8) as u8;
        buf[i + 2] = (word >> 16) as u8;
        buf[i + 3] = (word >> 24) as u8;
    }

    // Wait for data_done
    while !r.sr().read().data_done() {}
    r.sr().write(|w| w.set_data_done(true));
    Ok(())
}

fn write_block_hw(block_addr: u32, buf: &[u8; 512]) -> Result<(), SdmmcError> {
    let r = regs();

    // Data length = 512 bytes
    r.dlr().write(|w| w.set_data_len(0x1FF));

    // Configure write: tran_data_en, r_wn=write, block mode, 4-wire, start
    r.dcr().write(|w| {
        w.set_block_size(0x1FF);
        w.set_wire_mode(1);
        w.set_r_wn(false);
        w.set_tran_data_en(true);
        w.set_data_start(true);
    });

    // CMD24: WRITE_BLOCK
    send_cmd(24, block_addr, true, false)?;

    // Write 128 words to FIFO
    for i in (0..512).step_by(4) {
        let word = (buf[i] as u32)
            | ((buf[i + 1] as u32) << 8)
            | ((buf[i + 2] as u32) << 16)
            | ((buf[i + 3] as u32) << 24);
        r.fifo().write(|w| w.set_data(word));
    }

    // Wait for data_done
    while !r.sr().read().data_done() {}
    r.sr().write(|w| w.set_data_done(true));
    Ok(())
}

// ── IPC resource implementation ─────────────────────────────────────

struct SdmmcResource {
    block_count: u32,
}

impl Sdmmc for SdmmcResource {
    fn open(_meta: ipc::Meta) -> Result<Self, SdmmcOpenError> {
        if SDMMC_OPEN.swap(true, Ordering::Acquire) {
            return Err(SdmmcOpenError::ReservedSlot);
        }

        match init_card() {
            Ok(info) => Ok(SdmmcResource {
                block_count: info.block_count,
            }),
            Err(e) => {
                SDMMC_OPEN.store(false, Ordering::Release);
                Err(e)
            }
        }
    }

    fn read_block(
        &mut self,
        _meta: ipc::Meta,
        block: u32,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<(), BlockError> {
        if block >= self.block_count {
            return Err(BlockError::out_of_range());
        }

        // Caller must provide a full block buffer.
        if buf.len() < 512 {
            return Err(BlockError::device(0xFFFF));
        }

        let mut tmp = [0u8; 512];
        match read_block_hw(block, &mut tmp) {
            Ok(()) => {
                buf.write_range(0, &tmp);
                Ok(())
            }
            Err(e) => Err(BlockError::device(e as u16)),
        }
    }

    fn write_block(
        &mut self,
        _meta: ipc::Meta,
        block: u32,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<(), BlockError> {
        if block >= self.block_count {
            return Err(BlockError::out_of_range());
        }

        // Caller must provide a full block buffer.
        if buf.len() < 512 {
            return Err(BlockError::device(0xFFFF));
        }

        let mut tmp = [0u8; 512];
        buf.read_range(0, &mut tmp);
        match write_block_hw(block, &tmp) {
            Ok(()) => Ok(()),
            Err(e) => Err(BlockError::device(e as u16)),
        }
    }

    fn block_count(&mut self, _meta: ipc::Meta) -> u32 {
        self.block_count
    }
}

impl Drop for SdmmcResource {
    fn drop(&mut self) {
        SDMMC_OPEN.store(false, Ordering::Release);
    }
}

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");
    ipc::server! {
        Sdmmc: SdmmcResource,
    }
}
