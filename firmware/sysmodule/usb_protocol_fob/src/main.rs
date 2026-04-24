#![no_std]
#![no_main]

use generated::slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
use rcard_log::{info, ResultExt};
use sysmodule_usb_api::*;
use sysmodule_usb_protocol_fob_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup Reactor);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

sysmodule_usb_api::bind_usb_endpoint!(UsbEndpoint = SLOTS.sysmodule_usb);
sysmodule_usb_protocol_api::bind_usb_protocol_manager!(
    UsbProtocolManager = SLOTS.sysmodule_usb_protocol
);

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

static EP_IN: OnceCell<UsbEndpoint> = OnceCell::new();
static WRITER: GlobalState<rcard_usb_proto::FrameWriter> =
    GlobalState::new(rcard_usb_proto::FrameWriter::new());

// ---------------------------------------------------------------------------
// USB write helper
// ---------------------------------------------------------------------------

/// CRC-16/CCITT-FALSE, matching the host's counterpart in
/// `host/usb/src/crc.rs`. Used to compute the frame-level integrity check
/// appended to every outbound IPC frame.
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Emit one USB packet (≤ 64 bytes). Retries on `EndpointBusy`.
fn emit_packet(ep: &UsbEndpoint, data: &[u8]) -> Result<(), FobSendError> {
    loop {
        match ep.write(data) {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(UsbError::EndpointBusy)) => continue,
            Ok(Err(_)) => return Err(FobSendError::Disconnected),
            Err(e) => panic!("USB IPC died: {:?}", e),
        }
    }
}

/// Emit `data` as one USB bulk transfer containing the IPC frame plus a
/// trailing CRC16 (plus a 1-byte pad when the wire would otherwise be a
/// multiple of 64, to force a short-packet terminator).
///
/// The host validates CRC over `data` on receipt and strips it; any
/// corruption anywhere in the transfer fails CRC, the whole transfer is
/// discarded, and the next bulk transfer is by USB definition a fresh
/// frame. See `host/usb/src/crc.rs` for the matching unwrap.
fn write_usb(data: &[u8]) -> Result<(), FobSendError> {
    let ep = EP_IN.get().ok_or(FobSendError::Disconnected)?;
    let crc = crc16(data);
    let full = data.len() / 64;
    let tail = data.len() - full * 64;

    for i in 0..full {
        emit_packet(ep, &data[i * 64..(i + 1) * 64])?;
    }

    if tail > 0 {
        let mut last = [0u8; 64];
        last[..tail].copy_from_slice(&data[full * 64..]);
        last[tail] = (crc >> 8) as u8;
        last[tail + 1] = crc as u8;
        let total = tail + 2;
        if total <= 64 {
            emit_packet(ep, &last[..total])?;
            if total == 64 {
                // Wire is a multiple of 64 — emit a short pad packet.
                emit_packet(ep, &[0u8])?;
            }
        } else {
            // total == 65 (tail == 63). Split.
            emit_packet(ep, &last[..64])?;
            emit_packet(ep, &last[64..total])?;
        }
    } else {
        // `data.len()` is a multiple of 64 — emit CRC as a 2-byte short
        // terminator. Wire is `len + 2`, which is not a multiple of 64
        // (since `len % 64 == 0` and 2 ≠ 0), so no pad needed.
        emit_packet(ep, &[(crc >> 8) as u8, crc as u8])?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// UsbProtocolFob resource
// ---------------------------------------------------------------------------

struct FobResource;

impl UsbProtocolFob for FobResource {
    fn send(
        _meta: ipc::Meta,
        opcode: u8,
        payload: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<(), FobSendError> {
        // Bulk-copy lease data into a stack buffer.
        let mut data = [0u8; 500];
        let len = payload.len().min(data.len());
        let _ = payload.read_range(0, &mut data[..len]);

        // Encode and send.
        let mut buf = [0u8; 512];
        let n = WRITER
            .with(|w| w.write_simple_raw(opcode, &data[..len], &mut buf))
            .ok_or(FobSendError::BufferFull)?
            .ok_or(FobSendError::BufferFull)?;

        write_usb(&buf[..n])
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    let handles = UsbProtocolManager::take_fob_handles()
        .log_expect("manager IPC failed")
        .log_expect("take_fob_handles failed");

    info!("Opening fob channel endpoints");

    // Open OUT endpoint (for future host→fob messages on this channel).
    let _ep_out = UsbEndpoint::open(
        handles.ep_out,
        EndpointConfig {
            number: 2,
            direction: Direction::Out,
            transfer_type: TransferType::Bulk,
            max_packet_size: 64,
            interval: 0,
            interface_group: 1,
        },
    )
    .log_expect("EP OUT IPC failed")
    .log_expect("EP OUT open failed");

    // Open IN endpoint (fob→host).
    let ep_in = UsbEndpoint::open(
        handles.ep_in,
        EndpointConfig {
            number: 6,
            direction: Direction::In,
            transfer_type: TransferType::Bulk,
            max_packet_size: 64,
            interval: 0,
            interface_group: 1,
        },
    )
    .log_expect("EP IN IPC failed")
    .log_expect("EP IN open failed");

    EP_IN.set(ep_in).ok();

    // No spin-loop on bus_state — sysmodule_usb is IRQ-driven and the IN
    // endpoint will start accepting writes the moment USB is Configured.
    // Until then, the FobResource::send IPC handler will see EndpointBusy
    // / Disconnected from the underlying ep.write() and surface that to
    // the caller as FobSendError::Disconnected.
    info!("Fob channel ready, entering server loop");

    ipc::server! {
        UsbProtocolFob: FobResource,
    }
}
