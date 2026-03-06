//! File resource implementation.

use core::ffi::c_void;
use core::mem::MaybeUninit;

use littlefs2_sys::*;
use sysmodule_fs_api::{File, OpenError};

use crate::filesystem::lease_to_cstr;
use crate::state;

pub struct FileResource {
    fs_id: u8,
    file: lfs_file_t,
    file_cfg: lfs_file_config,
    file_cache: [u8; 512],
    path: [u8; 64],
}

impl FileResource {
    fn open_inner(
        fs_handle: ipc::DynHandle,
        path: &idyll_runtime::Leased<idyll_runtime::Read, u8>,
        lfs_flags: i32,
    ) -> Result<Self, OpenError> {
        log::info!(
            "open_inner: server_id={} kind=0x{:02x} handle={:?} flags=0x{:x}",
            fs_handle.server_id, fs_handle.kind, fs_handle.handle, lfs_flags,
        );
        // DIAGNOSTIC: resolve_fs fail → InvalidFs, tbl.get fail → Io
        let fs_id = state::resolve_fs(fs_handle.handle)
            .ok_or_else(|| {
                log::error!("open_inner: resolve_fs returned None for handle {:?}", fs_handle.handle);
                OpenError::InvalidFs
            })?;
        let tbl = unsafe { state::table() };
        let fs = tbl.get(fs_id).ok_or_else(|| {
            log::error!("open_inner: tbl.get({}) returned None", fs_id);
            OpenError::Io
        })?;

        let mut pathbuf = [0u8; 64];
        lease_to_cstr(path, &mut pathbuf);
        log::info!("open_inner: fs_id={}, path={:?}", fs_id, core::str::from_utf8(&pathbuf).unwrap_or("?"));

        let mut resource = FileResource {
            fs_id,
            file: unsafe { MaybeUninit::zeroed().assume_init() },
            file_cfg: unsafe { MaybeUninit::zeroed().assume_init() },
            file_cache: [0u8; 512],
            path: pathbuf,
        };

        resource.file_cfg.buffer = resource.file_cache.as_mut_ptr() as *mut c_void;
        resource.file_cfg.attr_count = 0;

        let rc = unsafe {
            lfs_file_opencfg(
                fs.lfs_ptr(),
                &mut resource.file,
                pathbuf.as_ptr() as *const core::ffi::c_char,
                lfs_flags,
                &resource.file_cfg,
            )
        };

        if rc != 0 {
            log::error!("open_inner: lfs_file_opencfg failed with rc={}", rc);
            return Err(lfs_err_to_open_error(rc));
        }

        log::info!("open_inner: file opened successfully");
        state::track_open(fs_id, &pathbuf);
        Ok(resource)
    }
}

impl File for FileResource {
    fn get(
        _meta: ipc::Meta,
        fs: ipc::DynHandle,
        path: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        // LFS_O_RDONLY
        Self::open_inner(fs, &path, 1)
    }

    fn get_or_create(
        _meta: ipc::Meta,
        fs: ipc::DynHandle,
        path: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        // LFS_O_RDWR | LFS_O_CREAT
        Self::open_inner(fs, &path, 3 | 0x0100)
    }

    fn read(
        &mut self,
        _meta: ipc::Meta,
        offset: u32,
        buf: idyll_runtime::Leased<idyll_runtime::Write, u8>,
    ) -> u32 {
        let tbl = unsafe { state::table() };
        let Some(fs) = tbl.get(self.fs_id) else {
            return 0;
        };

        unsafe { lfs_file_seek(fs.lfs_ptr(), &mut self.file, offset as i32, 0) };

        let mut total = 0u32;
        let mut tmp = [0u8; 256];
        let to_read = buf.len();
        while (total as usize) < to_read {
            let chunk = tmp.len().min(to_read - total as usize);
            let n = unsafe {
                lfs_file_read(
                    fs.lfs_ptr(),
                    &mut self.file,
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
        let tbl = unsafe { state::table() };
        let Some(fs) = tbl.get(self.fs_id) else {
            return 0;
        };

        unsafe { lfs_file_seek(fs.lfs_ptr(), &mut self.file, offset as i32, 0) };

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
                    &mut self.file,
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
        let tbl = unsafe { state::table() };
        let Some(fs) = tbl.get(self.fs_id) else {
            return 0;
        };
        let sz = unsafe { lfs_file_size(fs.lfs_ptr(), &mut self.file) };
        if sz >= 0 { sz as u32 } else { 0 }
    }

    fn unlink(&mut self, _meta: ipc::Meta) {
        state::track_unlink(self.fs_id, &self.path);
    }

    fn close(self, _meta: ipc::Meta) {
        let tbl = unsafe { state::table() };
        if let Some(fs) = tbl.get(self.fs_id) {
            let mut file = self.file;
            unsafe { lfs_file_close(fs.lfs_ptr(), &mut file) };
        }
        state::track_close(self.fs_id, &self.path);
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
