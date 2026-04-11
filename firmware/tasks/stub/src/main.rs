#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use generated::slots::SLOTS;
use once_cell::OnceCell;
use rcard_log::{info, OptionExt, ResultExt};
use sysmodule_usb_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup Reactor, UsbBus);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

sysmodule_usb_api::bind_usb_bus!(UsbBus = SLOTS.sysmodule_usb);
sysmodule_usb_api::bind_usb_endpoint!(UsbEndpoint = SLOTS.sysmodule_usb);
sysmodule_device_info_api::bind_device_info!(DeviceInfo = SLOTS.sysmodule_device_info);

// ── Test mode selection ─────────────────────────────────────────────
//
// Flip this constant to pick what the stub does on each usb_event wake:
//   - `Echo`: drain EP2 OUT and write it verbatim to EP1 IN (the original
//     round-trip echo used by the host test_speed.py scripts).
//   - `RxVerify`: drain EP2 OUT and compare each packet against the known
//     counter pattern `0, 1, 2, ...`. Logs every mismatch via USART. Never
//     writes to EP1 IN; use `test_speed.py --rx-only` on the host to pump
//     OUT packets and watch the device log for `stub rx BAD` lines.
//   - `TxGenerate`: push the counter pattern into EP1 IN as fast as the
//     FIFO accepts it. Never reads EP2 OUT. Use `test_speed.py --tx-only`
//     on the host to continuously read IN and verify.
//
// The three modes let us split round-trip corruption into RX-side vs
// TX-side vs echo-specific.
#[allow(unused)]
#[derive(Copy, Clone)]
enum Mode {
    Echo,
    RxVerify,
    TxGenerate,
}

const MODE: Mode = Mode::TxGenerate;

// Endpoints are stored as statics so the notification handler can access
// them. They're set once at startup and then read-only.
static EP_OUT: OnceCell<UsbEndpoint> = OnceCell::new();
static EP_IN: OnceCell<UsbEndpoint> = OnceCell::new();

// Cumulative counters (for periodic logging in RxVerify / TxGenerate).
static RX_BAD: AtomicU32 = AtomicU32::new(0);
static RX_OK: AtomicU32 = AtomicU32::new(0);
static TX_SENT: AtomicU32 = AtomicU32::new(0);

#[ipc::notification_handler(usb_event)]
fn handle_usb_event(_sender: u16, _code: u32) {
    match MODE {
        Mode::Echo => echo_handler(),
        Mode::RxVerify => rx_verify_handler(),
        Mode::TxGenerate => tx_generate_handler(),
    }
}

/// Drain OUT, echo back to IN unchanged.
fn echo_handler() {
    let Some(ep_out) = EP_OUT.get() else { return };
    let Some(ep_in) = EP_IN.get() else { return };

    let mut buf = [0u8; 64];
    loop {
        let n = match ep_out.read(&mut buf) {
            Ok(Ok(n)) if n > 0 => n as usize,
            _ => break,
        };

        // Write the full chunk back, retrying on EndpointBusy.
        let mut offset = 0;
        while offset < n {
            match ep_in.write(&buf[offset..n]) {
                Ok(Ok(written)) => offset += written as usize,
                Ok(Err(UsbError::EndpointBusy)) => continue,
                Ok(Err(UsbError::Disconnected)) => return,
                Ok(Err(_)) => return,
                Err(_) => return,
            }
        }
    }
}

/// Drain OUT, verify each packet against `buf[i] == i`. Logs the first
/// differing byte per bad packet and maintains cumulative OK/BAD counters.
fn rx_verify_handler() {
    let Some(ep_out) = EP_OUT.get() else { return };

    let mut buf = [0u8; 64];
    loop {
        let n = match ep_out.read(&mut buf) {
            Ok(Ok(n)) if n > 0 => n as usize,
            _ => break,
        };

        // Expected counter pattern: byte i = i as u8.
        let mut bad_idx: Option<usize> = None;
        for i in 0..n {
            if buf[i] != (i as u8) {
                bad_idx = Some(i);
                break;
            }
        }

        match bad_idx {
            None => {
                let c = RX_OK.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
                if c.is_multiple_of(10_000) {
                    info!("{} {}", "stub rx ok", c);
                }
            }
            Some(i) => {
                let bad_count = RX_BAD.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
                info!(
                    "{} {} {} {}",
                    "stub rx BAD", bad_count, i as u16, buf[i] as u16,
                );
            }
        }
    }
}

/// Push the counter pattern into IN until the FIFO reports EndpointBusy.
fn tx_generate_handler() {
    let Some(ep_in) = EP_IN.get() else { return };

    let payload: [u8; 64] = core::array::from_fn(|i| i as u8);
    loop {
        match ep_in.write(&payload) {
            Ok(Ok(_)) => {
                let c = TX_SENT.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
                if c.is_multiple_of(10_000) {
                    info!("{} {}", "stub tx sent", c);
                }
            }
            Ok(Err(UsbError::EndpointBusy)) => break,
            Ok(Err(UsbError::Disconnected)) => return,
            Ok(Err(_)) => return,
            Err(_) => return,
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    info!("{}", "stub: awake");

    // Build the MSOS 2.0 descriptor tree. A single Compatible ID at device
    // scope is enough to steer Windows onto WinUSB for the whole device.
    // Extend this builder with function subsets, registry properties, etc.
    // as the device grows.
    const MSOS_VENDOR_CODE: u8 = 0x01;
    let mut msos_buf = [0u8; 256];

    let msos_set = Msos20DescriptorSet::new(&mut msos_buf)
        .compatible_id("WINUSB", "")
        .build()
        .log_unwrap();

    let msos_platform = msos_platform_capability(msos_set.len() as u16, MSOS_VENDOR_CODE);

    // Serial number = hex(chip UID). sysmodule_device_info reads it once
    // from eFuse at its own startup, so the IPC here is a cheap cache hit.
    // The DeviceIdentity builder copies the string into identity_buf, so
    // serial_buf only has to outlive `.serial()`.
    let uid = DeviceInfo::get_uid().log_unwrap();
    let mut serial_buf = [0u8; sysmodule_device_info_api::UID_HEX_LEN];
    let serial = sysmodule_device_info_api::uid_to_hex(&uid, &mut serial_buf);

    // Take the USB bus and declare 2 endpoints (EP1 IN + EP1 OUT).
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

    let bus = UsbBus::take(identity.config(), identity.blob(), 2)
        .log_unwrap()
        .log_unwrap();

    info!("{}", "stub: bus taken");

    let h_out = bus
        .take_endpoint_handle()
        .log_unwrap()
        .log_expect("no handle");
    let h_in = bus
        .take_endpoint_handle()
        .log_unwrap()
        .log_expect("no handle");

    // Use EP2 OUT and EP1 IN (different endpoint numbers) to sidestep
    // the musb allocator's bulk IN+OUT-pair restriction.
    info!("{}", "stub: opening EP2 OUT");
    let ep_out = UsbEndpoint::open(
        h_out,
        EndpointConfig {
            number: 2,
            direction: Direction::Out,
            transfer_type: TransferType::Bulk,
            max_packet_size: 64,
            interval: 0,
        },
    )
    .log_unwrap()
    .log_unwrap();

    info!("{}", "stub: opening EP1 IN");
    let ep_in = UsbEndpoint::open(
        h_in,
        EndpointConfig {
            number: 1,
            direction: Direction::In,
            transfer_type: TransferType::Bulk,
            max_packet_size: 64,
            interval: 0,
        },
    )
    .log_unwrap()
    .log_unwrap();

    EP_OUT.set(ep_out).ok();
    EP_IN.set(ep_in).ok();

    match MODE {
        Mode::Echo => info!("{}", "stub: mode=echo"),
        Mode::RxVerify => info!("{}", "stub: mode=rx-verify"),
        Mode::TxGenerate => info!("{}", "stub: mode=tx-generate"),
    }

    // Bootstrap TxGenerate: the handler only runs on usb_event wakes, but
    // until the host actually reads EP1 IN there's no wake source for us.
    // Pre-fill the TX FIFO here so the first host read has data to return.
    if matches!(MODE, Mode::TxGenerate) {
        tx_generate_handler();
    }

    // Hold the bus alive — dropping it detaches USB. The server loop wakes
    // on usb_event notifications and runs the selected handler.
    ipc::server! {
        @notifications(Reactor) => handle_usb_event,
    }
}
