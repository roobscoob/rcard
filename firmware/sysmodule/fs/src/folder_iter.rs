//! FolderIterator resource implementation.

use littlefs2_sys::*;
use sysmodule_fs_api::{DirEntry, EntryType, FolderIterator};

use crate::folder::FolderResource;
use crate::state;

pub struct FolderIteratorResource {
    fs_id: u8,
    slot: usize,
    generation: u32,
}

impl FolderIterator<FolderResource> for FolderIteratorResource {
    fn iter(_meta: ipc::Meta, folder: &FolderResource) -> Self {
        FolderIteratorResource {
            fs_id: folder.fs_id(),
            slot: folder.dir_slot(),
            generation: folder.generation(),
        }
    }

    fn next(&mut self, _meta: ipc::Meta) -> Option<DirEntry> {
        state::with_state(|s| {
            let dir_entry = &mut s.open_dirs[self.slot];

            // Check that the slot is still occupied and the generation matches.
            if !dir_entry.occupied || dir_entry.generation != self.generation {
                return None;
            }

            let fs = s.fs_table.get(self.fs_id)?;

            let mut info: lfs_info = unsafe { core::mem::zeroed() };
            let rc = unsafe { lfs_dir_read(fs.lfs_ptr(), &mut dir_entry.dir, &mut info) };

            if rc <= 0 {
                return None;
            }

            let mut entry = DirEntry::EMPTY;

            let name_ptr = info.name.as_ptr();
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
        })
    }
}
