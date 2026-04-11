#![no_std]
#![no_main]

mod c_stubs;
mod file;
mod filesystem;
mod folder;
mod folder_iter;
mod registry;
mod state;

use generated::slots::SLOTS;

use sysmodule_fs_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup StoragePartition);
sysmodule_storage_api::bind_partition!(StoragePartition = SLOTS.sysmodule_storage);

struct AutoMount {
    name: &'static str,
    partition: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/filesystem_config.rs"));

/// Copy a short string into a null-padded [u8; 16].
fn name_to_buf(s: &str) -> [u8; 16] {
    let mut buf = [0u8; 16];
    let len = s.len().min(15);
    buf[..len].copy_from_slice(&s.as_bytes()[..len]);
    buf
}

fn auto_mount_filesystems() {
    for entry in AUTO_MOUNT {
        let part_name = name_to_buf(entry.partition);
        let storage = match StoragePartition::acquire(part_name) {
            Ok(Ok(handle)) => storage_api::StorageDyn::from_dyn_handle(handle.into()),
            Ok(Err(e)) => {
                rcard_log::error!(
                    "Automount Failed to acquire partition '{}': {}",
                    entry.partition,
                    e
                );

                continue;
            }
            Err(e) => {
                rcard_log::error!(
                    "Automount Failed to acquire partition '{}': {}",
                    entry.partition,
                    e
                );

                continue;
            }
        };

        let fs_id = match state::with_state(|s| s.fs_table.mount(storage)) {
            Ok(id) => id,
            Err(e) => {
                rcard_log::error!(
                    "Automount Failed to mount filesystem on partition '{}': {}",
                    entry.partition,
                    e
                );

                continue;
            }
        };

        let reg_name = name_to_buf(entry.name);
        if let Err(e) = registry::register_entry(reg_name, fs_id) {
            rcard_log::error!(
                "Automount Failed to register filesystem '{}': {}",
                entry.name,
                e
            );

            continue;
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");
    auto_mount_filesystems();

    ipc::server! {
        FileSystemRegistry: registry::RegistryResource,
        FileSystem: filesystem::FileSystemResource,
        File: file::FileResource,
        Folder: folder::FolderResource,
        FolderIterator: folder_iter::FolderIteratorResource,
    }
}
