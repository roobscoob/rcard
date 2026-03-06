//! FolderIterator resource implementation.

use core::mem::MaybeUninit;

use littlefs2_sys::*;
use sysmodule_fs_api::{DirEntry, EntryType, FolderIterator};

use crate::state;

pub struct FolderIteratorResource {
    fs_id: u8,
    /// Raw pointer to the lfs_dir_t inside the parent FolderResource (in its arena).
    dir_ptr: *mut lfs_dir_t,
}

impl FolderIterator for FolderIteratorResource {
    fn iter(_meta: ipc::Meta, folder: ipc::DynHandle) -> Self {
        let (fs_id, dir_ptr) = state::resolve_folder(folder.handle)
            .unwrap_or((0, core::ptr::null_mut()));
        FolderIteratorResource { fs_id, dir_ptr }
    }

    fn next(&mut self, _meta: ipc::Meta) -> Option<DirEntry> {
        if self.dir_ptr.is_null() {
            return None;
        }

        let tbl = unsafe { state::table() };
        let fs = tbl.get(self.fs_id)?;

        let mut info: lfs_info = unsafe { MaybeUninit::zeroed().assume_init() };
        let rc = unsafe { lfs_dir_read(fs.lfs_ptr(), self.dir_ptr, &mut info) };

        if rc <= 0 {
            return None;
        }

        let mut entry = DirEntry::EMPTY;

        let name_ptr = info.name.as_ptr() as *const u8;
        let mut i = 0;
        while i < 31 {
            let byte = unsafe { *name_ptr.add(i) };
            if byte == 0 {
                break;
            }
            entry.name[i] = byte;
            i += 1;
        }
        entry.name_len = i as u8;
        entry.size = info.size as u32;
        entry.entry_type = if info.type_ == 2 {
            EntryType::Directory
        } else {
            EntryType::File
        };

        Some(entry)
    }
}
