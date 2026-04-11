#![no_std]
#![no_main]

use generated::slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
use rcard_log::{info, OptionExt, ResultExt};
use sysmodule_usb_api::*;
use sysmodule_usb_protocol_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

sysmodule_usb_api::bind_usb_bus!(UsbBus = SLOTS.sysmodule_usb);

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct HandleState {
    host_handles: Option<HandlePair>,
    fob_handles: Option<HandlePair>,
}

static HANDLES: GlobalState<HandleState> = GlobalState::new(HandleState {
    host_handles: None,
    fob_handles: None,
});

/// The USB bus client handle. Stored here so `bus_state()` can call
/// `bus.state()`, which triggers a USB poll in the USB sysmodule.
/// Must stay alive — dropping it detaches USB.
static BUS: OnceCell<UsbBus> = OnceCell::new();

// ---------------------------------------------------------------------------
// UsbProtocolManager resource
// ---------------------------------------------------------------------------

struct ManagerResource;

impl UsbProtocolManager for ManagerResource {
    fn take_host_handles(_meta: ipc::Meta) -> Result<HandlePair, ManagerError> {
        HANDLES
            .with(|s| s.host_handles.take().ok_or(ManagerError::AlreadyTaken))
            .unwrap_or(Err(ManagerError::NotReady))
    }

    fn take_fob_handles(_meta: ipc::Meta) -> Result<HandlePair, ManagerError> {
        HANDLES
            .with(|s| s.fob_handles.take().ok_or(ManagerError::AlreadyTaken))
            .unwrap_or(Err(ManagerError::NotReady))
    }

    fn bus_state(_meta: ipc::Meta) -> BusState {
        // Each call to bus.state() makes an IPC call to the USB sysmodule,
        // which runs usb.poll() — this drives USB enumeration and data
        // transfer. Channel tasks poll this in a loop to bring the bus
        // from Detached → Configured.
        BUS.get()
            .and_then(|bus| bus.state().ok())
            .unwrap_or(BusState::Detached)
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    let bus = UsbBus::take(
        DeviceIdentity::new(0x16D0, 0x14EF)
            .device_class(0xFF, 0x01, 0x00)
            .manufacturer("Rose Kodsi-Hall")
            .product("Charm")
            .serial_number("BB01")
            .windows_driver("WINUSB"),
        4,
    )
    .log_unwrap()
    .log_unwrap();

    info!("USB bus taken, getting endpoint handles");

    let h_host_out = bus.take_endpoint_handle().log_unwrap().log_expect("no handle");
    let h_host_in = bus.take_endpoint_handle().log_unwrap().log_expect("no handle");
    let h_fob_out = bus.take_endpoint_handle().log_unwrap().log_expect("no handle");
    let h_fob_in = bus.take_endpoint_handle().log_unwrap().log_expect("no handle");

    HANDLES
        .with(|s| {
            s.host_handles = Some(HandlePair {
                ep_out: h_host_out,
                ep_in: h_host_in,
            });
            s.fob_handles = Some(HandlePair {
                ep_out: h_fob_out,
                ep_in: h_fob_in,
            });
        })
        .log_unwrap();

    // Store the bus so bus_state() can poll it.
    // The bus must stay alive — if it drops, USB detaches.
    BUS.set(bus).ok();

    info!("Handles ready, entering server loop");

    ipc::server! {
        UsbProtocolManager: ManagerResource,
    }
}
