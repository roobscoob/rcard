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
    header: [u8; HEADER_SIZE],
    stack_base: *const u8,
    stack_len: usize,
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

    let sp: u32;
    let lr: u32;
    let r0: u32;
    let r1: u32;
    let r2: u32;
    let r3: u32;
    let r4: u32;
    let r5: u32;
    let r6: u32;
    let r7: u32;
    let r8: u32;
    let r9: u32;
    let r10: u32;
    let r11: u32;
    let r12: u32;
    let xpsr: u32;

    // SAFETY: reading registers.  #[inline(never)] ensures the register
    // state reflects the caller's context (modulo prologue saves, which
    // the host recovers via DWARF CFI).
    unsafe {
        core::arch::asm!(
            "mov {sp}, sp",
            "mov {lr}, lr",
            "mrs {xpsr}, xpsr",
            sp = out(reg) sp,
            lr = out(reg) lr,
            xpsr = out(reg) xpsr,
            out("r0") r0,
            out("r1") r1,
            out("r2") r2,
            out("r3") r3,
            out("r4") r4,
            out("r5") r5,
            out("r6") r6,
            out("r7") r7,
            out("r8") r8,
            out("r9") r9,
            out("r10") r10,
            out("r11") r11,
            out("r12") r12,
            options(nomem, nostack, preserves_flags),
        );
    }

    let stack_top = unsafe { &_stack_start as *const u8 as u32 };
    let pc = lr;

    let fields: [u32; 18] = [
        sp, stack_top, lr, pc,
        r0, r1, r2, r3, r4, r5, r6, r7, r8, r9, r10, r11, r12,
        xpsr,
    ];

    let mut header = [0u8; HEADER_SIZE];
    for (i, &val) in fields.iter().enumerate() {
        header[i * 4..i * 4 + 4].copy_from_slice(&val.to_le_bytes());
    }

    let stack_len = stack_top.saturating_sub(sp) as usize;

    StackDump {
        header,
        stack_base: sp as *const u8,
        stack_len,
    }
}
