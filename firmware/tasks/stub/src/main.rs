#![no_std]
#![no_main]

use hubris_task_slots::SLOTS;
use rcard_log::{debug, error, info, warn, OptionExt, ResultExt};
use sysmodule_usb_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

sysmodule_usb_api::bind_usb_bus!(UsbBus = SLOTS.sysmodule_usb);
sysmodule_usb_api::bind_usb_endpoint!(UsbEndpoint = SLOTS.sysmodule_usb);

#[export_name = "main"]
fn main() -> ! {
    info!("hello from stub!");

    // Claim the USB bus as a vendor-specific device with 2 endpoints
    let bus = UsbBus::take(
        DeviceIdentity {
            vendor_id: 0x1209, // pid.codes open-source VID
            product_id: 0x0001,
            device_class: 0xFF, // vendor-specific
            device_subclass: 0x00,
            device_protocol: 0x00,
            bcd_device: 0x0100,
        },
        2,
    )
    .log_unwrap()
    .log_unwrap();

    info!("USB bus acquired");

    // Get endpoint handles
    let h_in = bus.take_endpoint_handle().log_unwrap().log_unwrap();
    let h_out = bus.take_endpoint_handle().log_unwrap().log_unwrap();

    // Open a bulk IN endpoint (device → host) on EP5 (TX-only)
    let ep_in = UsbEndpoint::open(
        h_in,
        EndpointConfig {
            number: 5,
            direction: Direction::In,
            transfer_type: TransferType::Bulk,
            max_packet_size: 64,
            interval: 0,
        },
    )
    .log_unwrap()
    .log_unwrap();

    // Open a bulk OUT endpoint (host → device) on EP2 (RX-only)
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

    info!("USB endpoints configured, waiting for host");

    // Poll until the host finishes enumeration
    loop {
        if bus.state().log_unwrap() == BusState::Configured {
            break;
        }
    }

    info!(
        "USB configured: {}, entering echo loop",
        bus.state().log_unwrap()
    );

    // Simple echo: read from OUT, write back to IN
    let mut buf = [0u8; 64];
    loop {
        match ep_out.read(&mut buf) {
            Ok(Ok(n)) => {
                let n = n as usize;
                if n > 0 {
                    debug!("USB received {} bytes, echoing back", n);

                    match ep_in.write(&buf[..n]) {
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => error!("USB write error: {}", e),
                        Err(e) => error!("USB write IPC error: {}", e),
                    }
                }
            }
            Ok(Err(UsbError::EndpointBusy)) => {
                info!("USB endpoint busy, retrying");
            }
            Ok(Err(UsbError::Disconnected)) => {
                // Bus lost configuration, wait for re-enumeration
                warn!("USB disconnected, waiting for host");
                loop {
                    if bus.state().log_unwrap() == BusState::Configured {
                        break;
                    }
                }
                info!("USB re-configured");
            }
            Ok(Err(e)) => error!("USB read error: {}", e),
            Err(e) => error!("USB read IPC error: {}", e),
        }
    }
}
