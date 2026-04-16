#![no_std]
#![no_main]

use generated::slots::SLOTS;
use ipc::kern::{sys_refresh_task_id, Gen};
use once_cell::GlobalState;
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
    /// Generation of `sysmodule_usb` when handles were created.
    usb_gen: Gen,
}

static HANDLES: GlobalState<HandleState> = GlobalState::new(HandleState {
    host_handles: None,
    fob_handles: None,
    usb_gen: Gen::DEFAULT,
});

/// The USB bus client handle. Stored here so `bus_state()` can call
/// `bus.state()`, which triggers a USB poll in the USB sysmodule.
/// Must stay alive — dropping it detaches USB.
static BUS: GlobalState<Option<UsbBus>> = GlobalState::new(None);

// ---------------------------------------------------------------------------
// USB setup buffers
// ---------------------------------------------------------------------------

/// Static buffers for USB descriptor construction. Keeps ~800 bytes off the
/// stack so `setup_usb` can be called from the ipc::server! dispatch path
/// without overflowing the 4 KiB task stack.
struct SetupBufs {
    msos: [u8; 256],
    serial: [u8; sysmodule_device_info_api::UID_HEX_LEN],
    identity: [u8; 512],
}

static SETUP_BUFS: GlobalState<SetupBufs> = GlobalState::new(SetupBufs {
    msos: [0u8; 256],
    serial: [0u8; sysmodule_device_info_api::UID_HEX_LEN],
    identity: [0u8; 512],
});

// ---------------------------------------------------------------------------
// USB setup
// ---------------------------------------------------------------------------

/// (Re-)create the USB bus and endpoint handles.
///
/// Called once at startup and again lazily if `sysmodule_usb` has restarted
/// since the handles were last created (detected by generation mismatch).
fn setup_usb() {
    info!("Setting up USB bus");

    let bus = SETUP_BUFS.with(|bufs| {
        const MSOS_VENDOR_CODE: u8 = 0x01;
        let msos_set = Msos20DescriptorSet::new(&mut bufs.msos)
            .compatible_id("WINUSB", "")
            .build()
            .log_unwrap();
        let msos_platform = msos_platform_capability(msos_set.len() as u16, MSOS_VENDOR_CODE);

        let uid = DeviceInfo::get_uid().log_unwrap();
        let serial = sysmodule_device_info_api::uid_to_hex(&uid, &mut bufs.serial);

        let identity = DeviceIdentity::builder(&mut bufs.identity, 0x16D0, 0x14EF)
            .device_class(0xFF, 0x01, 0x00)
            .manufacturer("Rose Kodsi-Hall")
            .product("Charm")
            .serial(serial)
            .bos_capability(0x05, &msos_platform)
            .vendor_request(MSOS_VENDOR_CODE, MSOS_DESCRIPTOR_INDEX, msos_set)
            .build()
            .log_unwrap();

        UsbBus::take(identity.config(), identity.blob(), 4)
            .log_unwrap()
            .log_unwrap()
    }).log_unwrap();

    let h_host_out = bus.take_endpoint_handle().log_unwrap().log_expect("no handle");
    let h_host_in = bus.take_endpoint_handle().log_unwrap().log_expect("no handle");
    let h_fob_out = bus.take_endpoint_handle().log_unwrap().log_expect("no handle");
    let h_fob_in = bus.take_endpoint_handle().log_unwrap().log_expect("no handle");

    let gen = sys_refresh_task_id(SLOTS.sysmodule_usb).generation();

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
            s.usb_gen = gen;
        })
        .log_unwrap();

    // Store the bus so bus_state() can poll it.
    // Replaces any stale handle from a previous sysmodule_usb generation.
    BUS.with(|b| { *b = Some(bus); }).log_unwrap();
}

/// If `sysmodule_usb` has restarted since we last created handles,
/// re-run the full USB setup to get fresh ones.
fn ensure_handles_fresh() {
    let current_gen = sys_refresh_task_id(SLOTS.sysmodule_usb).generation();
    let stale = HANDLES
        .with(|s| s.usb_gen != current_gen)
        .unwrap_or(true);
    if stale {
        setup_usb();
    }
}

// ---------------------------------------------------------------------------
// UsbProtocolManager resource
// ---------------------------------------------------------------------------

struct ManagerResource;

impl UsbProtocolManager for ManagerResource {
    fn take_host_handles(_meta: ipc::Meta) -> Result<HandlePair, ManagerError> {
        ensure_handles_fresh();
        HANDLES
            .with(|s| s.host_handles.take().ok_or(ManagerError::AlreadyTaken))
            .unwrap_or(Err(ManagerError::NotReady))
    }

    fn take_fob_handles(_meta: ipc::Meta) -> Result<HandlePair, ManagerError> {
        ensure_handles_fresh();
        HANDLES
            .with(|s| s.fob_handles.take().ok_or(ManagerError::AlreadyTaken))
            .unwrap_or(Err(ManagerError::NotReady))
    }

    fn bus_state(_meta: ipc::Meta) -> BusState {
        // Each call to bus.state() makes an IPC call to the USB sysmodule,
        // which runs usb.poll() — this drives USB enumeration and data
        // transfer. Channel tasks poll this in a loop to bring the bus
        // from Detached → Configured.
        BUS.with(|b| b.as_ref().and_then(|bus| bus.state().ok()))
            .flatten()
            .unwrap_or(BusState::Detached)
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    setup_usb();

    info!("Handles ready, entering server loop");

    ipc::server! {
        UsbProtocolManager: ManagerResource,
    }
}
