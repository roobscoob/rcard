//! NVDS (Non-Volatile Data Store) shared-memory init.
//!
//! Writes the default BLE TLV blob (BD address, scheduling, tracer) into
//! `0x2040_FE00` so the LCPU ROM picks it up at boot. Mirrors
//! `sifli-radio/src/bluetooth/nvds.rs::write_default()` and the SDK
//! `bt_stack_nvds_init()`.

use core::ptr;

use crate::addr;

const NVDS_PATTERN: u32 = 0x4E56_4453; // "NVDS"

mod tag {
    pub const BD_ADDRESS: u8 = 0x01;
    pub const PRE_WAKEUP_TIME: u8 = 0x0D;
    pub const EXT_WAKEUP_ENABLE: u8 = 0x12;
    pub const SCHEDULING: u8 = 0x15;
    pub const TRACER_CONFIG: u8 = 0x2F;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct NvdsHeader {
    pattern: u32,
    used_mem: u16,
    writing: u16,
}

/// Write the default NVDS blob to `0x2040_FE00`. Must be called with LCPU
/// held in reset (CPUWAIT high) so LCPU doesn't observe a partial write.
///
/// `use_lxt = true` skips the RC10K wake-up-timing tags. We default to
/// LXT (32 kHz crystal) — the only mode the bentoboard wears — but the
/// flag is exposed in case a future board uses RC10K.
pub fn write_default(bd_addr: &[u8; 6], use_lxt: bool) {
    let mut buf = [0u8; 64];
    let mut pos = 0usize;

    if !use_lxt {
        // Tag 0x0D: PRE_WAKEUP_TIME = 0x1964 (6500 µs) for RC10K mode.
        buf[pos..pos + 4].copy_from_slice(&[tag::PRE_WAKEUP_TIME, 0x02, 0x64, 0x19]);
        pos += 4;
        buf[pos..pos + 3].copy_from_slice(&[tag::EXT_WAKEUP_ENABLE, 0x01, 0x01]);
        pos += 3;
    }

    // Tag 0x2F: TRACER_CONFIG = 0x20 (4-byte payload).
    buf[pos..pos + 6].copy_from_slice(&[tag::TRACER_CONFIG, 0x04, 0x20, 0x00, 0x00, 0x00]);
    pos += 6;

    // Tag 0x01: BD address (6 bytes, little-endian).
    buf[pos] = tag::BD_ADDRESS;
    buf[pos + 1] = 0x06;
    buf[pos + 2..pos + 8].copy_from_slice(bd_addr);
    pos += 8;

    // Tag 0x15: SCHEDULING = 1.
    buf[pos..pos + 3].copy_from_slice(&[tag::SCHEDULING, 0x01, 0x01]);
    pos += 3;

    // Volatile writes — this region is shared with LCPU.
    let header = NvdsHeader {
        pattern: NVDS_PATTERN,
        used_mem: pos as u16,
        writing: 0,
    };
    unsafe {
        ptr::write_volatile(addr::NVDS_BUFF_START as *mut NvdsHeader, header);
        let payload_dst =
            (addr::NVDS_BUFF_START + core::mem::size_of::<NvdsHeader>()) as *mut u8;
        ptr::copy_nonoverlapping(buf.as_ptr(), payload_dst, pos);
    }
}
