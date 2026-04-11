#![no_std]
#![no_main]

use generated::slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
use rcard_log::{info, ResultExt};
use sysmodule_usb_api::*;
use sysmodule_usb_protocol_fob_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

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

fn write_usb(data: &[u8]) -> Result<(), FobSendError> {
    let ep = EP_IN.get().ok_or(FobSendError::Disconnected)?;
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + 64).min(data.len());
        match ep.write(&data[offset..end]) {
            Ok(Ok(n)) => offset += n as usize,
            Ok(Err(UsbError::EndpointBusy)) => continue,
            Ok(Err(UsbError::Disconnected)) => return Err(FobSendError::Disconnected),
            _ => return Err(FobSendError::Disconnected),
        }
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
        },
    )
    .log_expect("EP OUT IPC failed")
    .log_expect("EP OUT open failed");

    // Open IN endpoint (fob→host).
    let ep_in = UsbEndpoint::open(
        handles.ep_in,
        EndpointConfig {
            number: 2,
            direction: Direction::In,
            transfer_type: TransferType::Bulk,
            max_packet_size: 64,
            interval: 0,
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
