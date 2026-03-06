#![no_std]
#![no_main]

use core::mem::MaybeUninit;
use hubris_task_slots::SLOTS;

mod c_stubs;
mod file;
mod filesystem;
mod folder;
mod folder_iter;
mod registry;
mod state;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
sysmodule_log_api::panic_handler!(Log);

use sysmodule_fs_api::{
    FileDispatcher, FileSystemDispatcher, FileSystemRegistryDispatcher, FolderDispatcher,
    FolderIteratorDispatcher,
};

#[export_name = "main"]
fn main() -> ! {
    sysmodule_log_api::init_logger!(Log);
    log::info!("fs server v2 started");
    let mut registry_dispatcher = FileSystemRegistryDispatcher::<registry::RegistryResource>::new();
    let mut fs_dispatcher = FileSystemDispatcher::<filesystem::FileSystemResource>::new();
    let mut file_dispatcher = FileDispatcher::<file::FileResource>::new();
    let mut folder_dispatcher = FolderDispatcher::<folder::FolderResource>::new();
    let mut folder_iter_dispatcher =
        FolderIteratorDispatcher::<folder_iter::FolderIteratorResource>::new();

    // Register arena pointers so resources can cross-reference each other.
    unsafe {
        state::set_fs_arena(&fs_dispatcher.arena);
        state::set_folder_arena(&folder_dispatcher.arena);
    }

    let mut buf = [MaybeUninit::uninit(); 256];

    ipc::Server::<5>::new()
        .with_dispatcher(0x14, &mut registry_dispatcher)
        .with_dispatcher(0x12, &mut fs_dispatcher)
        .with_dispatcher(0x13, &mut file_dispatcher)
        .with_dispatcher(0x15, &mut folder_dispatcher)
        .with_dispatcher(0x16, &mut folder_iter_dispatcher)
        .run(&mut buf)
}
