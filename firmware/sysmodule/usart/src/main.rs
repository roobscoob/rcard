#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use generated::notifications;
use generated::slots::SLOTS;
use once_cell::GlobalState;
use sifli_pac::usart::{vals::M, Usart as UsartPeri};
use sysmodule_reactor_api::OverflowStrategy;
use sysmodule_usart_api::*;

sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

static USART_IN_USE: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

// ---------------------------------------------------------------------------
// USART2 RX ring buffer
//
// USART2 is the host-channel USART used by sysmodule_log (both log output
// and the host→device tunneled IPC path). The @irq handler drains the
// hardware RDR into this ring; IPC read() pulls from it.
//
// USART3 is TX-only (kernel debug output), so no ring is maintained there.
// ---------------------------------------------------------------------------

const RX_RING_SIZE: usize = 512;

struct RxRing {
    buf: [u8; RX_RING_SIZE],
    /// Next write position.
    head: usize,
    /// Next read position. `head == tail` => empty.
    tail: usize,
}

impl RxRing {
    const fn new() -> Self {
        Self {
            buf: [0; RX_RING_SIZE],
            head: 0,
            tail: 0,
        }
    }

    fn push(&mut self, b: u8) {
        let next = (self.head + 1) % RX_RING_SIZE;
        if next == self.tail {
            // Ring full — drop the oldest byte. Overflow means the host
            // is outrunning sysmodule_log's drain rate; losing tail bytes
            // matches how hardware ORE works in practice.
            self.tail = (self.tail + 1) % RX_RING_SIZE;
        }
        self.buf[self.head] = b;
        self.head = next;
    }

    fn pop(&mut self) -> Option<u8> {
        if self.head == self.tail {
            return None;
        }
        let b = self.buf[self.tail];
        self.tail = (self.tail + 1) % RX_RING_SIZE;
        Some(b)
    }
}

static USART2_RX: GlobalState<RxRing> = GlobalState::new(RxRing::new());

// ---------------------------------------------------------------------------
// Peripheral setup
// ---------------------------------------------------------------------------

fn usart_instance(index: u8) -> Option<UsartPeri> {
    match index {
        2 => Some(sifli_pac::USART2),
        3 => Some(sifli_pac::USART3),
        _ => None,
    }
}

fn init_usart(index: u8, regs: UsartPeri) {
    // BRR = 48MHz / 921600 = 52 (0x34)
    regs.brr().write(|w| w.0 = 0x34);
    regs.cr1().write(|w| {
        w.set_m(M::Bit8);
        w.set_ue(true);
        w.set_te(true);
        // Host channel: enable RX + RXNE + IDLE interrupts. RXNE
        // drains each byte into the ring buffer as it arrives (preventing
        // overrun). IDLE fires once after the last byte of a burst —
        // that's when we push the notification, instead of per-byte.
        if index == 2 {
            w.set_re(true);
            w.set_rxneie(true);
            w.set_idleie(true);
        }
    });
}

// ---------------------------------------------------------------------------
// Usart resource impl
// ---------------------------------------------------------------------------

struct UsartResource {
    index: u8,
    regs: UsartPeri,
}

impl Usart for UsartResource {
    fn open(_meta: ipc::Meta, index: u8) -> Result<Self, UsartOpenError> {
        if index == 1 {
            return Err(UsartOpenError::ReservedUsart);
        }

        let Some(regs) = usart_instance(index) else {
            return Err(UsartOpenError::InvalidIndex);
        };

        if USART_IN_USE[(index - 2) as usize].swap(true, Ordering::Acquire) {
            return Err(UsartOpenError::AlreadyOpen);
        }

        init_usart(index, regs);

        Ok(UsartResource { index, regs })
    }

    fn write(
        &mut self,
        _meta: ipc::Meta,
        data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) {
        // Chunk reads so we pay one syscall per ~128 bytes rather than
        // one per byte. Abort on lease read failure — the sender's lease
        // is gone, so silently injecting zeros would corrupt the stream.
        let mut chunk = [0u8; 128];
        let mut offset = 0;
        let total = data.len();
        while offset < total {
            let want = (total - offset).min(chunk.len());
            let Some(got) = data.read_range(offset, &mut chunk[..want]) else {

                return;
            };
            if got == 0 {

                return;
            }
            for &b in &chunk[..got] {
                while !self.regs.isr().read().txe() {}
                self.regs.tdr().write(|w| w.0 = b as u32);
            }
            offset += got;
        }

    }

    fn read(
        &mut self,
        _meta: ipc::Meta,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> u16 {
        // Only USART2 maintains an RX ring. USART3 (kernel debug) stays
        // TX-only; its read() always returns 0.
        if self.index != 2 {
            return 0;
        }
        let cap = buf.len();
        let mut chunk = [0u8; 128];
        let mut written = 0usize;
        USART2_RX.with(|ring| {
            while written < cap {
                let want = (cap - written).min(chunk.len());
                let mut filled = 0;
                while filled < want {
                    match ring.pop() {
                        Some(b) => {
                            chunk[filled] = b;
                            filled += 1;
                        }
                        None => break,
                    }
                }
                if filled == 0 {
                    break;
                }
                if buf.write_range(written, &chunk[..filled]).is_none() {
                    break;
                }
                written += filled;
                if filled < want {
                    break;
                }
            }
        });
        written as u16
    }
}

impl Drop for UsartResource {
    fn drop(&mut self) {
        USART_IN_USE[(self.index - 2) as usize].store(false, Ordering::Release);
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    userlib::sys_panic(b"usart panic")
}

#[export_name = "main"]
fn main() -> ! {
    ipc::server! {
        Usart: UsartResource,
        @irq(usart2_irq) => || {
            let regs = sifli_pac::USART2;

            // Check for overrun BEFORE draining — ORE is cleared by
            // reading ISR then RDR, so we must capture it first.
            if regs.isr().read().ore() {
                regs.icr().write(|w| w.set_orecf(true));
            }

            // Drain all pending bytes into the ring. RXNE fires per-byte
            // to prevent overrun, but we don't notify here.
            loop {
                let isr = regs.isr().read();
                if !isr.rxne() {
                    break;
                }
                let b = regs.rdr().read().rdr() as u8;
                let _ = USART2_RX.with(|ring| ring.push(b));
            }

            // Re-read ISR after draining — IDLE may have set during
            // the drain loop if the burst ended while we were reading.
            // Checking the fresh ISR instead of the pre-drain snapshot
            // avoids relying on a second ISR re-entry.
            if regs.isr().read().idle() {
                regs.icr().write(|w| w.set_idlecf(true));
                let _ = Reactor::refresh(
                    notifications::GROUP_ID_USART_EVENT,
                    2,
                    15,
                    OverflowStrategy::DropOldest,
                );
            }
        },
    }
}
