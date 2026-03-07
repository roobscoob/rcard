//! File resource implementation.
//!
//! The heavy littlefs state (`lfs_file_t`, cache, config) lives in a static
//! table so it is never moved after `lfs_file_opencfg`.  `FileResource` is
//! just a thin index into that table and can be freely moved by the IPC arena.

use core::ffi::c_void;
use core::mem::MaybeUninit;

use littlefs2_sys::*;
use sysmodule_fs_api::{File, OpenError};

use crate::filesystem::{lease_to_cstr, FileSystemResource};
use crate::state;

const MAX_OPEN_FILES: usize = 4;

struct OpenFile {
    file: lfs_file_t,
    file_cfg: lfs_file_config,
    file_cache: [u8; 512],
    path: [u8; 64],
    fs_id: u8,
    refcount: u8,
    occupied: bool,
}

static mut OPEN_FILES: [OpenFile; MAX_OPEN_FILES] = {
    const EMPTY: OpenFile = OpenFile {
        file: unsafe { MaybeUninit::zeroed().assume_init() },
        file_cfg: unsafe { MaybeUninit::zeroed().assume_init() },
        file_cache: [0u8; 512],
        path: [0u8; 64],
        fs_id: 0,
        refcount: 0,
        occupied: false,
    };
    [EMPTY; MAX_OPEN_FILES]
};

fn open_files() -> &'static mut [OpenFile; MAX_OPEN_FILES] {
    unsafe { &mut *core::ptr::addr_of_mut!(OPEN_FILES) }
}

/// Find an already-open file with the same fs_id and path, bump refcount.
fn find_existing(fs_id: u8, path: &[u8; 64]) -> Option<usize> {
    let tbl = open_files();
    let idx = tbl
        .iter()
        .position(|f| f.occupied && f.fs_id == fs_id && f.path == *path)?;
    tbl[idx].refcount = tbl[idx].refcount.saturating_add(1);
    Some(idx)
}

/// Open a new file in the static table. Returns the slot index.
fn open_new(fs_id: u8, path: &[u8; 64], lfs_flags: i32) -> Result<usize, OpenError> {
    let tbl = open_files();
    let idx = tbl.iter().position(|f| !f.occupied).ok_or_else(|| {
        log::error!("open_new: no free file slots");
        OpenError::Io
    })?;

    let fs_tbl = unsafe { state::table() };
    let mounted = fs_tbl.get(fs_id).ok_or_else(|| {
        log::error!("open_new: fs not mounted (fs_id={})", fs_id);
        OpenError::Io
    })?;

    let slot = &mut tbl[idx];
    slot.file = unsafe { MaybeUninit::zeroed().assume_init() };
    slot.file_cfg = unsafe { MaybeUninit::zeroed().assume_init() };
    slot.file_cache = [0u8; 512];
    slot.path = *path;
    slot.fs_id = fs_id;

    // These pointers are stable because `slot` lives in a static.
    slot.file_cfg.buffer = slot.file_cache.as_mut_ptr() as *mut c_void;
    slot.file_cfg.attr_count = 0;

    let rc = unsafe {
        lfs_file_opencfg(
            mounted.lfs_ptr(),
            &mut slot.file,
            slot.path.as_ptr() as *const core::ffi::c_char,
            lfs_flags,
            &slot.file_cfg,
        )
    };

    if rc != 0 {
        log::error!("open_new: lfs_file_opencfg failed with rc={}", rc);
        return Err(lfs_err_to_open_error(rc));
    }

    slot.refcount = 1;
    slot.occupied = true;
    state::track_open(fs_id, path);
    log::info!("open_new: file slot {} opened", idx);
    Ok(idx)
}

pub struct FileResource {
    slot: usize,
}

impl FileResource {
    fn open_inner(
        fs: &FileSystemResource,
        path: &idyll_runtime::Leased<idyll_runtime::Read, u8>,
        lfs_flags: i32,
    ) -> Result<Self, OpenError> {
        let fs_id = fs.fs_id;

        let mut pathbuf = [0u8; 64];
        lease_to_cstr(path, &mut pathbuf);
        let path_len = pathbuf.iter().position(|&b| b == 0).unwrap_or(pathbuf.len());
        log::info!(
            "open_inner: fs_id={}, path={:?}, flags=0x{:x}",
            fs_id,
            core::str::from_utf8(&pathbuf[..path_len]).unwrap_or("?"),
            lfs_flags,
        );

        let slot = match find_existing(fs_id, &pathbuf) {
            Some(idx) => {
                log::info!("open_inner: reusing file slot {}", idx);
                idx
            }
            None => open_new(fs_id, &pathbuf, lfs_flags)?,
        };

        Ok(FileResource { slot })
    }

    /// Open a file by fs_id directly (used by scheme-path constructors).
    fn open_by_id(fs_id: u8, pathbuf: &[u8; 64], lfs_flags: i32) -> Result<Self, OpenError> {
        let path_len = pathbuf.iter().position(|&b| b == 0).unwrap_or(pathbuf.len());
        log::info!(
            "open_by_id: fs_id={}, path={:?}, flags=0x{:x}",
            fs_id,
            core::str::from_utf8(&pathbuf[..path_len]).unwrap_or("?"),
            lfs_flags,
        );

        let slot = match find_existing(fs_id, pathbuf) {
            Some(idx) => {
                log::info!("open_by_id: reusing file slot {}", idx);
                idx
            }
            None => open_new(fs_id, pathbuf, lfs_flags)?,
        };

        Ok(FileResource { slot })
    }

    fn entry(&mut self) -> &mut OpenFile {
        &mut open_files()[self.slot]
    }
}

/// Parse a `scheme:/path` lease into (fs_id, path_portion).
///
/// Reads the lease into `buf`, splits on the first `:`, looks up the scheme
/// in the filesystem registry, and returns the fs_id plus the path portion
/// (starting from the `/` after the `:`).
fn parse_scheme_path(
    lease: &idyll_runtime::Leased<idyll_runtime::Read, u8>,
    buf: &mut [u8; 64],
) -> Result<(u8, usize), OpenError> {
    use crate::registry::lookup_by_name;

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

    let fs_id = lookup_by_name(&name).ok_or(OpenError::InvalidFs)?;

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
        path: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        Self::open_inner(fs, &path, 3)
    }

    fn get(
        _meta: ipc::Meta,
        path: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        let mut pathbuf = [0u8; 64];
        let (fs_id, _) = parse_scheme_path(&path, &mut pathbuf)?;
        Self::open_by_id(fs_id, &pathbuf, 3)
    }

    fn get_or_create_in(
        _meta: ipc::Meta,
        fs: &FileSystemResource,
        path: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        Self::open_inner(fs, &path, 3 | 0x0100)
    }

    fn get_or_create(
        _meta: ipc::Meta,
        path: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        let mut pathbuf = [0u8; 64];
        let (fs_id, _) = parse_scheme_path(&path, &mut pathbuf)?;
        Self::open_by_id(fs_id, &pathbuf, 3 | 0x0100)
    }

    fn read(
        &mut self,
        _meta: ipc::Meta,
        offset: u32,
        buf: idyll_runtime::Leased<idyll_runtime::Write, u8>,
    ) -> u32 {
        let entry = self.entry();
        let tbl = unsafe { state::table() };
        let Some(fs) = tbl.get(entry.fs_id) else {
            return 0;
        };

        unsafe { lfs_file_seek(fs.lfs_ptr(), &mut entry.file, offset as i32, 0) };

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
    }

    fn write(
        &mut self,
        _meta: ipc::Meta,
        offset: u32,
        buf: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> u32 {
        let entry = self.entry();
        let tbl = unsafe { state::table() };
        let Some(fs) = tbl.get(entry.fs_id) else {
            return 0;
        };

        unsafe { lfs_file_seek(fs.lfs_ptr(), &mut entry.file, offset as i32, 0) };

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
    }

    fn size(&mut self, _meta: ipc::Meta) -> u32 {
        let entry = self.entry();
        let tbl = unsafe { state::table() };
        let Some(fs) = tbl.get(entry.fs_id) else {
            return 0;
        };
        let sz = unsafe { lfs_file_size(fs.lfs_ptr(), &mut entry.file) };
        if sz >= 0 { sz as u32 } else { 0 }
    }

    fn unlink(&mut self, _meta: ipc::Meta) {
        let entry = self.entry();
        state::track_unlink(entry.fs_id, &entry.path);
    }

    fn close(self, _meta: ipc::Meta) {
        // Drop impl handles cleanup.
    }
}

impl Drop for FileResource {
    fn drop(&mut self) {
        let tbl = open_files();
        let slot = &mut tbl[self.slot];
        slot.refcount = slot.refcount.saturating_sub(1);
        if slot.refcount == 0 {
            let fs_tbl = unsafe { state::table() };
            if let Some(fs) = fs_tbl.get(slot.fs_id) {
                unsafe { lfs_file_close(fs.lfs_ptr(), &mut slot.file) };
            }
            let path = slot.path;
            let fs_id = slot.fs_id;
            slot.occupied = false;
            state::track_close(fs_id, &path);
        }
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
