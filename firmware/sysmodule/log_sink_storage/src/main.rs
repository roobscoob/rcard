#![no_std]
#![no_main]

use hubris_task_slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
use rcard_log::{OptionExt, ResultExt};
use sysmodule_storage_api::{partitions, ring::RingWriter};

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup Reactor, Partition);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);
sysmodule_storage_api::bind_partition!(Partition = SLOTS.sysmodule_storage);

mod generated {
    include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
}

/// Last seen log entry ID for `consume_since`.
static LAST_ID: GlobalState<u64> = GlobalState::new(0);

/// Ring writer for the logs partition.
static WRITER: OnceCell<GlobalState<RingWriter>> = OnceCell::new();

/// Drain new log entries from the log sysmodule and write them to storage.
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

        // Parse entries from the buffer and write each to the ring.
        // Wire format: [id:8][len:1][idx:1][data:len]
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

            WRITER
                .get()
                .log_expect("writer not initialized")
                .with(|w| {
                    w.begin();
                    w.write(&buf[offset..offset + HEADER + data_len]);
                    w.end();
                })
                .log_unwrap();

            last = id;
            LAST_ID.with(|stored| *stored = last).log_unwrap();
            offset += HEADER + data_len;
            count += 1;
        }
    }

    count
}

#[ipc::notification_handler(logs)]
fn handle_logs(_sender: u16, _code: u32) {
    drain_logs();
}

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");

    // Acquire the "logs" partition.
    let partition = Partition::acquire(partitions::LOGS)
        .log_expect("failed to acquire logs partition")
        .log_expect("failed to acquire logs partition");

    rcard_log::trace!("Acquired logs partition");

    // Initialize the ring writer.
    let storage = storage_api::StorageDyn::from_dyn_handle(partition.into());
    WRITER.set(GlobalState::new(RingWriter::new(storage))).ok();

    rcard_log::trace!("Initialized ring writer");

    // Do an initial drain in case entries accumulated before we started.
    let initial_logs = drain_logs();

    rcard_log::trace!(
        "Drained {} initial logs; entering server loop",
        initial_logs
    );

    ipc::server! {
        @notifications(Reactor) => handle_logs,
    }
}
