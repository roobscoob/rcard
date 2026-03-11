//! File resource implementation.
//!
//! The heavy littlefs state (`lfs_file_t`, cache, config) lives in the global
//! `FsState` table so it is never moved after `lfs_file_opencfg`.  `FileResource`
//! is just a thin index into that table and can be freely moved by the IPC arena.

use core::ffi::c_void;

use littlefs2_sys::*;
use sysmodule_fs_api::{File, FileOffset, OpenError};

use crate::filesystem::{lease_to_cstr, FileSystemResource};
use crate::state;

/// Find an already-open file with the same fs_id and path, bump refcount.
fn find_existing(s: &mut state::FsState, fs_id: u8, path: &[u8; 64], _lfs_flags: i32) -> Option<usize> {
    let idx = s
        .open_files
        .iter()
        .position(|f| f.occupied && f.fs_id == fs_id && f.path == *path)?;
    s.open_files[idx].refcount = s.open_files[idx].refcount.saturating_add(1);
    Some(idx)
}

/// Open a new file in the static table. Returns the slot index.
fn open_new(s: &mut state::FsState, fs_id: u8, path: &[u8; 64], lfs_flags: i32) -> Result<usize, OpenError> {
    let idx = s
        .open_files
        .iter()
        .position(|f| !f.occupied)
        .ok_or(OpenError::Io)?;

    let mounted = s.fs_table.get(fs_id).ok_or(OpenError::Io)?;
    let lfs_ptr = mounted.lfs_ptr();

    let slot = &mut s.open_files[idx];
    slot.file = unsafe { core::mem::zeroed() };
    slot.file_cfg = unsafe { core::mem::zeroed() };
    slot.file_cache = [0u8; 512];
    slot.path = *path;
    slot.fs_id = fs_id;
    slot.lfs_flags = lfs_flags;

    // These pointers are stable because `slot` lives in a static.
    slot.file_cfg.buffer = slot.file_cache.as_mut_ptr() as *mut c_void;
    slot.file_cfg.attr_count = 0;

    let rc = unsafe {
        lfs_file_opencfg(
            lfs_ptr,
            &mut slot.file,
            slot.path.as_ptr() as *const core::ffi::c_char,
            lfs_flags,
            &slot.file_cfg,
        )
    };

    if rc != 0 {
        return Err(lfs_err_to_open_error(rc));
    }

    slot.refcount = 1;
    slot.occupied = true;
    slot.unlinked = false;
    Ok(idx)
}

pub struct FileResource {
    slot: usize,
}

impl FileResource {
    fn open_inner(
        fs: &FileSystemResource,
        path: &ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
        lfs_flags: i32,
    ) -> Result<Self, OpenError> {
        let fs_id = fs.fs_id;

        let mut pathbuf = [0u8; 64];
        lease_to_cstr(path, &mut pathbuf);

        state::with_state(|s| {
            let slot = match find_existing(s, fs_id, &pathbuf, lfs_flags) {
                Some(idx) => idx,
                None => open_new(s, fs_id, &pathbuf, lfs_flags)?,
            };

            Ok(FileResource { slot })
        })
    }

    /// Open a file by fs_id directly (used by scheme-path constructors).
    fn open_by_id(fs_id: u8, pathbuf: &[u8; 64], lfs_flags: i32) -> Result<Self, OpenError> {
        state::with_state(|s| {
            let slot = match find_existing(s, fs_id, pathbuf, lfs_flags) {
                Some(idx) => idx,
                None => open_new(s, fs_id, pathbuf, lfs_flags)?,
            };

            Ok(FileResource { slot })
        })
    }
}

/// Parse a `scheme:/path` lease into (fs_id, path_portion).
///
/// Reads the lease into `buf`, splits on the first `:`, looks up the scheme
/// in the filesystem registry, and returns the fs_id plus the path portion
/// (starting from the `/` after the `:`).
fn parse_scheme_path(
    lease: &ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    buf: &mut [u8; 64],
) -> Result<(u8, usize), OpenError> {
    let len = lease.len().min(buf.len());
    for i in 0..len {
        buf[i] = lease.read(i).unwrap_or(0);
    }
    for i in len..buf.len() {
        buf[i] = 0;
    }

    let colon = buf[..len]
        .iter()
        .position(|&b| b == b':')
        .ok_or(OpenError::NotFound)?;

    // Build the registry name from the scheme portion
    let mut name = [0u8; 16];
    let scheme_len = colon.min(16);
    name[..scheme_len].copy_from_slice(&buf[..scheme_len]);

    let fs_id = crate::registry::lookup_by_name(&name).ok_or(OpenError::InvalidFs)?;

    // Shift the path portion (after the colon) to the front of buf
    let path_start = colon + 1;
    let path_len = len - path_start;
    buf.copy_within(path_start..path_start + path_len, 0);
    buf[path_len] = 0; // null-terminate
    for i in (path_len + 1)..buf.len() {
        buf[i] = 0;
    }

    Ok((fs_id, path_len))
}

impl File<FileSystemResource> for FileResource {
    fn get_in(
        _meta: ipc::Meta,
        fs: &FileSystemResource,
        path: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<Self, OpenError> {
        Self::open_inner(fs, &path, 3)
    }

    fn get(
        _meta: ipc::Meta,
        path: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<Self, OpenError> {
        let mut pathbuf = [0u8; 64];
        let (fs_id, _) = parse_scheme_path(&path, &mut pathbuf)?;
        Self::open_by_id(fs_id, &pathbuf, 3)
    }

    fn get_or_create_in(
        _meta: ipc::Meta,
        fs: &FileSystemResource,
        path: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<Self, OpenError> {
        Self::open_inner(fs, &path, 3 | 0x0100)
    }

    fn get_or_create(
        _meta: ipc::Meta,
        path: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<Self, OpenError> {
        let mut pathbuf = [0u8; 64];
        let (fs_id, _) = parse_scheme_path(&path, &mut pathbuf)?;
        Self::open_by_id(fs_id, &pathbuf, 3 | 0x0100)
    }

    fn read(
        &mut self,
        _meta: ipc::Meta,
        offset: FileOffset,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> u32 {
        debug_assert!(offset.get() <= i32::MAX as u32);
        state::with_state(|s| {
            let entry = &mut s.open_files[self.slot];
            let fs_id = entry.fs_id;
            let Some(fs) = s.fs_table.get(fs_id) else {
                return 0;
            };

            unsafe { lfs_file_seek(fs.lfs_ptr(), &mut entry.file, offset.as_i32(), 0) };

            let mut total = 0u32;
            let mut tmp = [0u8; 256];
            let to_read = buf.len();
            while (total as usize) < to_read {
                let chunk = tmp.len().min(to_read - total as usize);
                let n = unsafe {
                    lfs_file_read(
                        fs.lfs_ptr(),
                        &mut entry.file,
                        tmp.as_mut_ptr() as *mut c_void,
                        chunk as u32,
                    )
                };
                if n <= 0 {
                    break;
                }
                for i in 0..n as usize {
                    let _ = buf.write(total as usize + i, tmp[i]);
                }
                total += n as u32;
            }
            total
        })
    }

    fn write(
        &mut self,
        _meta: ipc::Meta,
        offset: FileOffset,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> u32 {
        debug_assert!(offset.get() <= i32::MAX as u32);
        state::with_state(|s| {
            let entry = &mut s.open_files[self.slot];
            let fs_id = entry.fs_id;
            let Some(fs) = s.fs_table.get(fs_id) else {
                return 0;
            };

            unsafe { lfs_file_seek(fs.lfs_ptr(), &mut entry.file, offset.as_i32(), 0) };

            let mut total = 0u32;
            let mut tmp = [0u8; 256];
            let to_write = buf.len();
            while (total as usize) < to_write {
                let chunk = tmp.len().min(to_write - total as usize);
                for i in 0..chunk {
                    tmp[i] = buf.read(total as usize + i).unwrap_or(0);
                }
                let n = unsafe {
                    lfs_file_write(
                        fs.lfs_ptr(),
                        &mut entry.file,
                        tmp.as_ptr() as *const c_void,
                        chunk as u32,
                    )
                };
                if n <= 0 {
                    break;
                }
                total += n as u32;
            }
            total
        })
    }

    fn size(&mut self, _meta: ipc::Meta) -> u32 {
        state::with_state(|s| {
            let entry = &mut s.open_files[self.slot];
            let fs_id = entry.fs_id;
            let Some(fs) = s.fs_table.get(fs_id) else {
                return 0;
            };
            let sz = unsafe { lfs_file_size(fs.lfs_ptr(), &mut entry.file) };
            if sz >= 0 { sz as u32 } else { 0 }
        })
    }

    fn unlink(&mut self, _meta: ipc::Meta) {
        state::with_state(|s| {
            s.open_files[self.slot].unlinked = true;
        });
    }

    fn close(self, _meta: ipc::Meta) {
        // Drop impl handles cleanup.
    }
}

impl Drop for FileResource {
    fn drop(&mut self) {
        state::with_state(|s| {
            let slot = &mut s.open_files[self.slot];
            slot.refcount = slot.refcount.saturating_sub(1);
            if slot.refcount == 0 {
                if let Some(fs) = s.fs_table.get(slot.fs_id) {
                    unsafe { lfs_file_close(fs.lfs_ptr(), &mut slot.file) };
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

fn lfs_err_to_open_error(rc: i32) -> OpenError {
    match rc {
        -2 => OpenError::NotFound,
        -5 => OpenError::Io,
        -21 => OpenError::IsDirectory,
        -28 => OpenError::NoSpace,
        _ => OpenError::Io,
    }
}
