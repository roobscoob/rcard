//! FileSystem resource implementation.

use sysmodule_fs_api::{FileSystem, FileSystemError, FileSystemStats};

use crate::state;

pub struct FileSystemResource {
    pub fs_id: u8,
}

impl FileSystem for FileSystemResource {
    fn mount(_meta: ipc::Meta, storage: ipc::DynHandle) -> Result<Self, FileSystemError> {
        let dyn_storage = storage_api::StorageDyn::from_dyn_handle(storage);
        let fs_id = state::with_state(|s| s.fs_table.mount(dyn_storage))?;
        Ok(FileSystemResource { fs_id })
    }

    fn lookup(
        _meta: ipc::Meta,
        _registry: ipc::RawHandle,
        _name: [u8; 16],
    ) -> Option<Self> {
        // TODO: look up a previously-mounted filesystem by name via the registry.
        None
    }

    fn format(_meta: ipc::Meta, storage: ipc::DynHandle) -> Result<Self, FileSystemError> {
        let dyn_storage = storage_api::StorageDyn::from_dyn_handle(storage);
        let fs_id = state::with_state(|s| s.fs_table.format(dyn_storage))?;
        Ok(FileSystemResource { fs_id })
    }

    fn stat(&mut self, _meta: ipc::Meta) -> FileSystemStats {
        state::with_state(|s| {
            let fs = s.fs_table.get(self.fs_id).expect("fs: stat on invalid fs_id");
            let used = unsafe { littlefs2_sys::lfs_fs_size(fs.lfs_ptr()) };
            let used_blocks = if used >= 0 { used as u32 } else { 0 };
            FileSystemStats {
                total_blocks: fs.block_count,
                free_blocks: fs.block_count.saturating_sub(used_blocks),
                block_size: 512,
            }
        })
    }
}

impl Drop for FileSystemResource {
    fn drop(&mut self) {
        state::with_state(|s| s.fs_table.unmount(self.fs_id));
    }
}

/// Read a lease into a null-terminated stack buffer.
pub fn lease_to_cstr(
    lease: &idyll_runtime::Leased<idyll_runtime::Read, u8>,
    buf: &mut [u8; 64],
) -> usize {
    let len = lease.len().min(63);
    for i in 0..len {
        buf[i] = lease.read(i).unwrap_or(0);
    }
    buf[len] = 0;
    len
}
