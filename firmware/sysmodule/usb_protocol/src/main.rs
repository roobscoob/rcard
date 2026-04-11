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
sysmodule_device_info_api::bind_device_info!(DeviceInfo = SLOTS.sysmodule_device_info);

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

    // Build the MSOS 2.0 descriptor tree. A device-scope CompatibleID is
    // enough to bind WinUSB on Windows for the whole device. Mirrors the
    // shape used in the stub task harness.
    const MSOS_VENDOR_CODE: u8 = 0x01;
    let mut msos_buf = [0u8; 256];
    let msos_set = Msos20DescriptorSet::new(&mut msos_buf)
        .compatible_id("WINUSB", "")
        .build()
        .log_unwrap();
    let msos_platform = msos_platform_capability(msos_set.len() as u16, MSOS_VENDOR_CODE);

    // Serial number = hex(chip UID). sysmodule_device_info caches it from
    // eFuse at its own startup, so this IPC is cheap.
    let uid = DeviceInfo::get_uid().log_unwrap();
    let mut serial_buf = [0u8; sysmodule_device_info_api::UID_HEX_LEN];
    let serial = sysmodule_device_info_api::uid_to_hex(&uid, &mut serial_buf);

    let mut identity_buf = [0u8; 2048];
    let identity = DeviceIdentity::builder(&mut identity_buf, 0x16D0, 0x14EF)
        .device_class(0xFF, 0x01, 0x00)
        .manufacturer("Rose Kodsi-Hall")
        .product("Charm")
        .serial(serial)
        .bos_capability(0x05, &msos_platform)
        .vendor_request(MSOS_VENDOR_CODE, MSOS_DESCRIPTOR_INDEX, msos_set)
        .build()
        .log_unwrap();

    let bus = UsbBus::take(identity.config(), identity.blob(), 4)
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
