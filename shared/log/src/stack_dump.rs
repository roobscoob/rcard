//! Stack dump capture for post-mortem backtrace generation.
//!
//! The [`capture`] function snapshots the current register file and reads
//! stack bytes from SP to the linker-provided `_stack_start` symbol.  The
//! resulting [`StackDump`] implements [`Format`] and can be included in any
//! log message.  The host CLI decodes it and produces a backtrace using
//! DWARF debug info from the task ELF.

use crate::formatter::{Format, Formatter, Writer};

/// Header size: sp, stack_top, lr, pc, r0..r12, xpsr = 18 × u32 = 72 bytes.
pub const HEADER_SIZE: usize = 72;

/// A snapshot of the CPU register file and live stack contents.
///
/// Created by [`capture`].  Implements [`Format`] so it can be sent
/// through the structured logging pipeline like any other value.
pub struct StackDump {
    pub header: [u8; HEADER_SIZE],
    pub stack_base: *const u8,
    pub stack_len: usize,
}

// StackDump holds a raw pointer into the current task's stack, which is
// valid for the lifetime of the task and only used on the same core.
unsafe impl Send for StackDump {}

impl Format for StackDump {
    fn format<W: Writer>(&self, formatter: &mut Formatter<W>) {
        // SAFETY: stack_base..+stack_len is within our own task's stack.
        let stack = unsafe { core::slice::from_raw_parts(self.stack_base, self.stack_len) };

        formatter.write_stack_dump(&self.header, stack);
    }
}

/// Capture the current register file and stack contents.
///
/// Returns a [`StackDump`] that references the live stack by pointer
/// (zero-copy).  The `_stack_start` linker symbol must be defined
/// (provided by the standard Hubris task linker scripts).
#[inline(always)]
pub fn capture() -> StackDump {
    extern "C" {
        static _stack_start: u8;
    }

    let stack_top = unsafe { &_stack_start as *const u8 as u32 };

    let mut fields: [u32; 18] = [0; 18];
    fields[1] = stack_top;

    unsafe {
        let base = fields.as_mut_ptr();
        let tmp: u32;

        core::arch::asm!(
            "mov r12, {base}",

            // --- SP ---
            "str sp, [r12, #0]",

            // --- LR ---
            "mov {tmp}, lr",
            "str {tmp}, [r12, #8]",

            // --- PC (approximate, but good enough) ---
            "adr {tmp}, .",
            "str {tmp}, [r12, #12]",

            // --- r0–r11 ---
            "str r0, [r12, #16]",
            "str r1, [r12, #20]",
            "str r2, [r12, #24]",
            "str r3, [r12, #28]",
            "str r4, [r12, #32]",
            "str r5, [r12, #36]",
            "str r6, [r12, #40]",
            "str r7, [r12, #44]",
            "str r8, [r12, #48]",
            "str r9, [r12, #52]",
            "str r10, [r12, #56]",
            "str r11, [r12, #60]",

            // --- r12 (clobbered, but that's fine) ---
            "str r12, [r12, #64]",

            // --- xPSR ---
            "mrs {tmp}, xpsr",
            "str {tmp}, [r12, #68]",

            base = in(reg) base,
            tmp = lateout(reg) tmp,

            options(nostack, preserves_flags),
        );

        let _ = tmp;
    }

    let mut header = [0u8; HEADER_SIZE];
    for (i, &val) in fields.iter().enumerate() {
        header[i * 4..i * 4 + 4].copy_from_slice(&val.to_le_bytes());
    }

    let sp = fields[0];
    let stack_len = stack_top.saturating_sub(sp) as usize;

    StackDump {
        header,
        stack_base: sp as *const u8,
        stack_len,
    }
}
