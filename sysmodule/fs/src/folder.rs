//! Folder (directory) resource implementation.
//!
//! Like files, the heavy `lfs_dir_t` lives in a static table so it is never
//! moved after `lfs_dir_open`.

use core::mem::MaybeUninit;

use littlefs2_sys::*;
use sysmodule_fs_api::{Folder, OpenError};

use crate::filesystem::{lease_to_cstr, FileSystemResource};
use crate::state;

const MAX_OPEN_DIRS: usize = 4;

struct OpenDir {
    dir: lfs_dir_t,
    path: [u8; 64],
    fs_id: u8,
    refcount: u8,
    occupied: bool,
}

static mut OPEN_DIRS: [OpenDir; MAX_OPEN_DIRS] = {
    const EMPTY: OpenDir = OpenDir {
        dir: unsafe { MaybeUninit::zeroed().assume_init() },
        path: [0u8; 64],
        fs_id: 0,
        refcount: 0,
        occupied: false,
    };
    [EMPTY; MAX_OPEN_DIRS]
};

fn open_dirs() -> &'static mut [OpenDir; MAX_OPEN_DIRS] {
    unsafe { &mut *core::ptr::addr_of_mut!(OPEN_DIRS) }
}

fn find_existing(fs_id: u8, path: &[u8; 64]) -> Option<usize> {
    let tbl = open_dirs();
    let idx = tbl
        .iter()
        .position(|d| d.occupied && d.fs_id == fs_id && d.path == *path)?;
    tbl[idx].refcount = tbl[idx].refcount.saturating_add(1);
    Some(idx)
}

fn open_new(fs_id: u8, path: &[u8; 64]) -> Result<usize, OpenError> {
    let tbl = open_dirs();
    let idx = tbl.iter().position(|d| !d.occupied).ok_or(OpenError::Io)?;

    let fs_tbl = unsafe { state::table() };
    let mounted = fs_tbl.get(fs_id).ok_or(OpenError::InvalidFs)?;

    let slot = &mut tbl[idx];
    slot.dir = unsafe { MaybeUninit::zeroed().assume_init() };
    slot.path = *path;
    slot.fs_id = fs_id;

    let rc = unsafe {
        lfs_dir_open(
            mounted.lfs_ptr(),
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
    state::track_open(fs_id, path);
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

        let slot = match find_existing(fs_id, &pathbuf) {
            Some(idx) => idx,
            None => open_new(fs_id, &pathbuf)?,
        };

        Ok(FolderResource { slot })
    }

    pub fn fs_id(&self) -> u8 {
        open_dirs()[self.slot].fs_id
    }

    pub fn dir_ptr(&self) -> *mut lfs_dir_t {
        &mut open_dirs()[self.slot].dir as *mut _
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
                let tbl = unsafe { state::table() };
                let mounted = tbl.get(fs.fs_id).ok_or(OpenError::InvalidFs)?;

                let mut pathbuf = [0u8; 64];
                lease_to_cstr(&path, &mut pathbuf);

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

                Self::open_inner(fs, &path)
            }
            Err(e) => Err(e),
        }
    }
}

impl Drop for FolderResource {
    fn drop(&mut self) {
        let tbl = open_dirs();
        let slot = &mut tbl[self.slot];
        slot.refcount = slot.refcount.saturating_sub(1);
        if slot.refcount == 0 {
            let fs_tbl = unsafe { state::table() };
            if let Some(fs) = fs_tbl.get(slot.fs_id) {
                unsafe { lfs_dir_close(fs.lfs_ptr(), &mut slot.dir) };
            }
            let path = slot.path;
            let fs_id = slot.fs_id;
            slot.occupied = false;
            state::track_close(fs_id, &path);
        }
    }
}
