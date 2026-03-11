#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use hubris_task_slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
use sysmodule_storage_api::{partitions, ring::RingWriter};

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
sysmodule_log_api::panic_handler!(to Log; cleanup Reactor, Partition);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);
sysmodule_storage_api::bind_partition!(Partition = SLOTS.sysmodule_storage);

mod generated {
    include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
}

/// Last seen log entry ID for `consume_since`.
static LAST_ID: AtomicU32 = AtomicU32::new(0);

/// Ring writer for the logs partition.
static WRITER: OnceCell<GlobalState<RingWriter>> = OnceCell::new();

/// Drain new log entries from the log sysmodule and write them to storage.
fn drain_logs() {
    let mut last = LAST_ID.load(Ordering::Relaxed);
    let mut buf = [0u8; 512];

    loop {
        let n = Log::consume_since(last, &mut buf)
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(0) as usize;
        if n == 0 {
            break;
        }

        // Parse entries from the buffer and write each to the ring.
        // Wire format: [id:4][level:1][task:2][idx:2][time:8][len:2][data:len]
        const HEADER: usize = 19;
        let mut offset = 0;
        while offset + HEADER <= n {
            let id = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]);
            let data_len = u16::from_le_bytes([buf[offset + 17], buf[offset + 18]]) as usize;

            if offset + HEADER + data_len > n {
                break;
            }

            // Only persist entries at INFO level or above (level <= 3).
            let level = buf[offset + 4];
            if level <= 3 {
                WRITER.get().expect("writer not initialized").with(|w| {
                    w.begin();
                    w.write(&buf[offset..offset + HEADER + data_len]);
                    w.end();
                });
            }

            last = id;
            LAST_ID.store(id, Ordering::Relaxed);
            offset += HEADER + data_len;
        }
    }
}

#[ipc::notification_handler(logs)]
fn handle_logs(_sender: u16, _code: u32) {
    drain_logs();
}

#[export_name = "main"]
fn main() -> ! {
    // Acquire the "logs" partition.
    let partition = Partition::acquire(partitions::LOGS)
        .expect("failed to acquire logs partition")
        .expect("failed to acquire logs partition");

    // Initialize the ring writer.
    let storage = storage_api::StorageDyn::from_dyn_handle(partition.into());
    WRITER.set(GlobalState::new(RingWriter::new(storage))).ok();

    // Do an initial drain in case entries accumulated before we started.
    drain_logs();

    ipc::server! {
        @notifications(Reactor) => handle_logs,
    }
}
