#![no_std]
#![no_main]

use generated::slots::SLOTS;
use once_cell::OnceCell;
use sysmodule_device_api::*;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    userlib::sys_panic(b"device panic")
}

sysmodule_efuse_api::bind_efuse!(Efuse = SLOTS.sysmodule_efuse);
sysmodule_rand_api::bind_rand!(Rand = SLOTS.sysmodule_rand);

static UID: OnceCell<[u8; 16]> = OnceCell::new();
static SESSION_ID: OnceCell<[u8; 16]> = OnceCell::new();

struct DeviceImpl;

impl Device for DeviceImpl {
    fn get_uid(_meta: ipc::Meta) -> [u8; 16] {
        UID.get().copied().unwrap_or([0u8; 16])
    }

    fn get_session_id(_meta: ipc::Meta) -> [u8; 16] {
        SESSION_ID.get().copied().unwrap_or([0u8; 16])
    }

    fn reset(_meta: ipc::Meta) {
        kipc::reset();
    }
}

#[export_name = "main"]
fn main() -> ! {
    if let Ok(Ok(bank)) = Efuse::read(0) {
        let mut uid = [0u8; 16];
        uid.copy_from_slice(&bank[..16]);
        UID.set(uid).ok();
    }

    if let Ok(Ok(bytes)) = Rand::generate() {
        let mut sid = [0u8; 16];
        sid.copy_from_slice(&bytes[..16]);
        SESSION_ID.set(sid).ok();
    }

    ipc::server! {
        Device: DeviceImpl,
    }
}
