//! Folder (directory) resource implementation.
//!
//! Like files, the heavy `lfs_dir_t` lives in the global `FsState` table so
//! it is never moved after `lfs_dir_open`.

use littlefs2_sys::*;
use sysmodule_fs_api::{Folder, OpenError};

use crate::filesystem::{lease_to_cstr, FileSystemResource};
use crate::state;

fn find_existing(s: &mut state::FsState, fs_id: u8, path: &[u8; 64]) -> Option<usize> {
    let idx = s
        .open_dirs
        .iter()
        .position(|d| d.occupied && d.fs_id == fs_id && d.path == *path)?;
    s.open_dirs[idx].refcount = s.open_dirs[idx].refcount.saturating_add(1);
    Some(idx)
}

fn open_new(s: &mut state::FsState, fs_id: u8, path: &[u8; 64]) -> Result<usize, OpenError> {
    let idx = s
        .open_dirs
        .iter()
        .position(|d| !d.occupied)
        .ok_or(OpenError::Io)?;

    let mounted = s.fs_table.get(fs_id).ok_or(OpenError::InvalidFs)?;
    let lfs_ptr = mounted.lfs_ptr();

    let slot = &mut s.open_dirs[idx];
    slot.dir = unsafe { core::mem::zeroed() };
    slot.path = *path;
    slot.fs_id = fs_id;

    let rc = unsafe {
        lfs_dir_open(
            lfs_ptr,
            &mut slot.dir,
            slot.path.as_ptr() as *const core::ffi::c_char,
        )
    };

    if rc != 0 {
        return Err(match rc {
            -2 => OpenError::NotFound,
            _ => OpenError::Io,
        });
    }

    slot.refcount = 1;
    slot.occupied = true;
    slot.unlinked = false;
    slot.generation = slot.generation.wrapping_add(1);
    Ok(idx)
}

pub struct FolderResource {
    slot: usize,
}

impl FolderResource {
    fn open_inner(
        fs: &FileSystemResource,
        path: &idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        let fs_id = fs.fs_id;

        let mut pathbuf = [0u8; 64];
        lease_to_cstr(path, &mut pathbuf);

        state::with_state(|s| {
            let slot = match find_existing(s, fs_id, &pathbuf) {
                Some(idx) => idx,
                None => open_new(s, fs_id, &pathbuf)?,
            };

            Ok(FolderResource { slot })
        })
    }

    pub fn fs_id(&self) -> u8 {
        state::with_state(|s| s.open_dirs[self.slot].fs_id)
    }

    pub fn dir_slot(&self) -> usize {
        self.slot
    }

    pub fn generation(&self) -> u32 {
        state::with_state(|s| s.open_dirs[self.slot].generation)
    }
}

impl Folder<FileSystemResource> for FolderResource {
    fn get(
        _meta: ipc::Meta,
        fs: &FileSystemResource,
        path: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        Self::open_inner(fs, &path)
    }

    fn get_or_create(
        _meta: ipc::Meta,
        fs: &FileSystemResource,
        path: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        match Self::open_inner(fs, &path) {
            Ok(r) => Ok(r),
            Err(OpenError::NotFound) => {
                let fs_id = fs.fs_id;

                let mut pathbuf = [0u8; 64];
                lease_to_cstr(&path, &mut pathbuf);

                state::with_state(|s| {
                    let mounted = s.fs_table.get(fs_id).ok_or(OpenError::InvalidFs)?;

                    let rc = unsafe {
                        lfs_mkdir(
                            mounted.lfs_ptr(),
                            pathbuf.as_ptr() as *const core::ffi::c_char,
                        )
                    };
                    if rc != 0 {
                        return Err(match rc {
                            -2 => OpenError::NotFound,
                            _ => OpenError::Io,
                        });
                    }

                    let slot = open_new(s, fs_id, &pathbuf)?;
                    Ok(FolderResource { slot })
                })
            }
            Err(e) => Err(e),
        }
    }
}

impl Drop for FolderResource {
    fn drop(&mut self) {
        state::with_state(|s| {
            let slot = &mut s.open_dirs[self.slot];
            slot.refcount = slot.refcount.saturating_sub(1);
            if slot.refcount == 0 {
                if let Some(fs) = s.fs_table.get(slot.fs_id) {
                    unsafe { lfs_dir_close(fs.lfs_ptr(), &mut slot.dir) };
                }
                if slot.unlinked {
                    if let Some(fs) = s.fs_table.get(slot.fs_id) {
                        unsafe {
                            lfs_remove(
                                fs.lfs_ptr(),
                                slot.path.as_ptr() as *const core::ffi::c_char,
                            );
                        }
                    }
                }
                slot.occupied = false;
            }
        });
    }
}
