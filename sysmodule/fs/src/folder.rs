//! Folder (directory) resource implementation.

use core::mem::MaybeUninit;

use littlefs2_sys::*;
use sysmodule_fs_api::{OpenError, Folder};

use crate::filesystem::lease_to_cstr;
use crate::state;

pub struct FolderResource {
    pub fs_id: u8,
    pub dir: lfs_dir_t,
    path: [u8; 64],
}

impl FolderResource {
    fn open_inner(
        fs_handle: ipc::DynHandle,
        path: &idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        let fs_id = state::resolve_fs(fs_handle.handle)
            .ok_or(OpenError::InvalidFs)?;
        let tbl = unsafe { state::table() };
        let fs = tbl.get(fs_id).ok_or(OpenError::InvalidFs)?;

        let mut pathbuf = [0u8; 64];
        lease_to_cstr(path, &mut pathbuf);

        let mut resource = FolderResource {
            fs_id,
            dir: unsafe { MaybeUninit::zeroed().assume_init() },
            path: pathbuf,
        };

        let rc = unsafe {
            lfs_dir_open(
                fs.lfs_ptr(),
                &mut resource.dir,
                pathbuf.as_ptr() as *const core::ffi::c_char,
            )
        };

        if rc != 0 {
            return Err(match rc {
                -2 => OpenError::NotFound,
                _ => OpenError::Io,
            });
        }

        state::track_open(fs_id, &pathbuf);
        Ok(resource)
    }
}

impl Folder for FolderResource {
    fn get(
        _meta: ipc::Meta,
        fs: ipc::DynHandle,
        path: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        Self::open_inner(fs, &path)
    }

    fn get_or_create(
        _meta: ipc::Meta,
        fs: ipc::DynHandle,
        path: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) -> Result<Self, OpenError> {
        match Self::open_inner(fs, &path) {
            Ok(r) => Ok(r),
            Err(OpenError::NotFound) => {
                let fs_id = state::resolve_fs(fs.handle)
                    .ok_or(OpenError::InvalidFs)?;
                let tbl = unsafe { state::table() };
                let mounted = tbl.get(fs_id).ok_or(OpenError::InvalidFs)?;

                let mut pathbuf = [0u8; 64];
                lease_to_cstr(&path, &mut pathbuf);

                unsafe {
                    lfs_mkdir(
                        mounted.lfs_ptr(),
                        pathbuf.as_ptr() as *const core::ffi::c_char,
                    )
                };

                Self::open_inner(fs, &path)
            }
            Err(e) => Err(e),
        }
    }
}

impl Drop for FolderResource {
    fn drop(&mut self) {
        let tbl = unsafe { state::table() };
        if let Some(fs) = tbl.get(self.fs_id) {
            unsafe { lfs_dir_close(fs.lfs_ptr(), &mut self.dir) };
        }
        state::track_close(self.fs_id, &self.path);
    }
}
