#![no_std]

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

/// Failures of [`Lcpu::init`].
#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Schema,
    rcard_log::Format,
)]
#[repr(u8)]
pub enum LcpuInitError {
    /// LCPU is already in use by another task.
    AlreadyOpen = 0,
    /// HXT48 oscillator did not report ready within budget.
    Hxt48Timeout = 1,
    /// LP_LCPU / LP_MAC reset assertion or release timed out.
    ResetTimeout = 2,
    /// LCPU did not emit the warmup HCI event in time.
    WarmupTimeout = 3,
    /// First post-release frame was not an HCI Event (H4 type 0x04).
    WarmupBadFrame = 4,
    /// Patch installer rejected the bundled patch blob.
    PatchInstallFailed = 5,
}

/// Failures of [`Lcpu::send_hci`].
#[derive(
    Copy,
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Schema,
    rcard_log::Format,
)]
#[repr(u8)]
pub enum HciSendError {
    /// Frame larger than the IPC ring's available space.
    TooLarge = 0,
    /// Caller is not the current LCPU holder. Should be unreachable on a
    /// live handle but exists as a defensive check.
    NotHolder = 1,
}

/// IPC resource exposing the SF32LB52 LCPU (BLE/BT controller).
///
/// `qty1` — at most one task may hold an `Lcpu` handle. Acquiring the
/// handle drives the full bringup sequence (NVDS, ROM config, clocks,
/// patches, release, warmup HCI event, post-init). Dropping the handle
/// puts LCPU back in reset.
///
/// HCI traffic is exchanged over the SoC mailbox: `send_hci` doorbells
/// MAILBOX1; the LCPU's MAILBOX2 IRQ wakes the holder via a
/// caller-chosen notification mask, and the holder drains pending
/// frames with `recv_hci` until it returns 0.
#[ipc::resource(arena_size = 1, kind = 0x17)]
pub trait Lcpu {
    /// Bring up the LCPU and BLE controller, blocking until the warmup
    /// HCI event is received and post-init has run.
    ///
    /// `bd_addr` is the 6-byte little-endian Bluetooth Device Address
    /// the controller advertises. Written into NVDS tag `0x01`.
    ///
    /// `rx_notification_mask` is the notification bit (or set of bits)
    /// the lcpu task posts on the caller via `sys_post` whenever the
    /// LCPU→HCPU mailbox IRQ fires. The caller drains via [`recv_hci`].
    #[constructor]
    fn init(bd_addr: [u8; 6], rx_notification_mask: u32) -> Result<Self, LcpuInitError>;

    /// Push an HCI H4 frame (type byte + payload) onto the HCPU→LCPU
    /// ring and doorbell MAILBOX1. Returns once the bytes are queued —
    /// LCPU consumption is asynchronous.
    #[message]
    fn send_hci(&mut self, #[lease] data: &[u8]) -> Result<(), HciSendError>;

    /// Drain pending HCI bytes from the LCPU→HCPU ring into `buf`.
    /// Returns the number of bytes copied. Zero means the ring is
    /// currently empty; callers that want every byte should keep
    /// calling until they see zero, or use the next notification edge.
    #[message]
    fn recv_hci(&mut self, #[lease] buf: &mut [u8]) -> u16;
}
