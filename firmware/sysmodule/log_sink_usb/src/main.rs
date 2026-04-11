#![no_std]
#![no_main]

use generated::slots::SLOTS;
use once_cell::GlobalState;
use rcard_log::{info, trace, OptionExt};

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup Reactor);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);
sysmodule_usb_protocol_fob_api::bind_usb_protocol_fob!(
    UsbProtocolFob = SLOTS.sysmodule_usb_protocol_fob
);

/// Last seen log entry ID for `consume_since`.
static LAST_ID: GlobalState<u64> = GlobalState::new(0);

/// Drain new log entries from the log sysmodule and send them over USB.
fn drain_logs() -> u32 {
    let mut last = LAST_ID.with(|id| *id).log_unwrap();
    let mut buf = [0u8; 512];
    let mut count = 0;

    loop {
        let n = Log::consume_since(last, &mut buf)
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(0) as usize;

        if n == 0 {
            break;
        }

        // Walk entries to find the last log_id, then send the whole
        // batch as a single SimpleFrame.
        const HEADER: usize = 10;
        let mut offset = 0;
        while offset + HEADER <= n {
            let id = u64::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
                buf[offset + 4],
                buf[offset + 5],
                buf[offset + 6],
                buf[offset + 7],
            ]);
            let data_len = buf[offset + 8] as usize;

            if offset + HEADER + data_len > n {
                break;
            }

            last = id;
            offset += HEADER + data_len;
            count += 1;
        }

        // Send the batch over USB. If the fob channel is disconnected
        // or full, silently drop — logs are best-effort over USB.
        // We still advance LAST_ID to avoid unbounded buffering.
        let _ = UsbProtocolFob::send(
            rcard_usb_proto::messages::log_entry::OP_LOG_ENTRY,
            &buf[..offset],
        );

        LAST_ID.with(|stored| *stored = last).log_unwrap();
    }

    count
}

#[ipc::notification_handler(logs)]
fn handle_logs(_sender: u16, _code: u32) {
    drain_logs();
}

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    // Initial drain in case entries accumulated before we started.
    let initial = drain_logs();
    trace!("Drained {} initial logs; entering server loop", initial);

    ipc::server! {
        @notifications(Reactor) => handle_logs,
    }
}
