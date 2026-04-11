#![no_std]
#![no_main]

use generated::slots::SLOTS;
use once_cell::OnceCell;
use rcard_log::{info, ResultExt};
use sysmodule_device_info_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);
sysmodule_efuse_api::bind_efuse!(Efuse = SLOTS.sysmodule_efuse);

/// Cached chip UID, fetched from eFuse bank 0 at task startup.
///
/// eFuse reads are stable for the life of the chip, so we pay the IPC
/// cost exactly once and answer every subsequent `get_uid()` call from
/// the cache.
static UID: OnceCell<[u8; 16]> = OnceCell::new();

struct DeviceInfoImpl;

impl DeviceInfo for DeviceInfoImpl {
    fn get_uid(_meta: ipc::Meta) -> [u8; 16] {
        UID.get().copied().unwrap_or([0u8; 16])
    }
}

#[export_name = "main"]
fn main() -> ! {
    // Fetch the UID once. If efuse is unreachable (dead or mis-configured),
    // fall back to zero so the server still runs and clients see a
    // sentinel value instead of a hang.
    let bank = Efuse::read(0).log_unwrap().log_unwrap();
    let mut uid = [0u8; 16];
    uid.copy_from_slice(&bank[..16]);
    UID.set(uid).ok();

    info!("Awake");

    ipc::server! {
        DeviceInfo: DeviceInfoImpl,
    }
}
