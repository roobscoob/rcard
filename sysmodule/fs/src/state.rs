//! Global shared state for mounted filesystems.
//!
//! All resource implementations (FileSystem, File, Folder) access this shared
//! state.  Since we run in a single-threaded Hubris task, `unsafe` access to
//! the static is sound — there is no preemption within the server loop.

use core::cell::UnsafeCell;
use core::ffi::{c_int, c_void};
use core::mem::MaybeUninit;

use littlefs2_sys::*;
use storage_api::StorageDyn;

use crate::filesystem::FileSystemResource;
use crate::folder::FolderResource;

/// Maximum number of simultaneously mounted filesystems.
pub const MAX_FS: usize = 4;

/// Block size used by littlefs (matches typical SD sector size).
const BLOCK_SIZE: u32 = 512;
/// Read granularity.
const READ_SIZE: u32 = 512;
/// Write (program) granularity.
const PROG_SIZE: u32 = 512;
/// Cache size — must be a multiple of read/prog size and a divisor of block size.
const CACHE_SIZE: usize = 512;
/// Lookahead buffer size in bytes (must be a multiple of 8).
const LOOKAHEAD_SIZE: usize = 16;

/// Per-filesystem context stored alongside the lfs state.
pub struct MountedFs {
    pub lfs: UnsafeCell<lfs_t>,
    pub config: lfs_config,
    pub storage: StorageDyn,
    read_cache: [u8; CACHE_SIZE],
    prog_cache: [u8; CACHE_SIZE],
    lookahead_buf: [u8; LOOKAHEAD_SIZE],
    pub block_count: u32,
    pub mounted: bool,
}

/// Global table of mounted filesystems.
pub struct FsTable {
    slots: [Option<MountedFs>; MAX_FS],
}

/// The single global filesystem table.
///
/// SAFETY: only accessed from the single-threaded server loop.
static mut FS_TABLE: FsTable = FsTable::new();

/// Pointer to the FileSystem dispatcher's arena.
/// Set once in main() before the server loop starts.
static mut FS_ARENA: Option<*const ipc::Arena<FileSystemResource, 4>> = None;

/// Pointer to the Folder dispatcher's arena.
static mut FOLDER_ARENA: Option<*const ipc::Arena<FolderResource, 16>> = None;

impl FsTable {
    const fn new() -> Self {
        Self {
            slots: [const { None }; MAX_FS],
        }
    }
}

/// Get a reference to the global table.
///
/// SAFETY: must only be called from the server task's main loop (single-threaded).
pub unsafe fn table() -> &'static mut FsTable {
    unsafe { &mut *core::ptr::addr_of_mut!(FS_TABLE) }
}

/// Store a pointer to the FileSystem dispatcher's arena.
///
/// SAFETY: must be called once from main() before the server loop.
pub unsafe fn set_fs_arena(arena: &ipc::Arena<FileSystemResource, 4>) {
    unsafe { FS_ARENA = Some(arena as *const _) };
}

/// Store a pointer to the Folder dispatcher's arena.
pub unsafe fn set_folder_arena(arena: &ipc::Arena<FolderResource, 16>) {
    unsafe { FOLDER_ARENA = Some(arena as *const _) };
}

/// Resolve a FileSystem DynHandle to an fs_id by looking up the arena.
pub fn resolve_fs(handle: ipc::RawHandle) -> Option<u8> {
    let arena_ptr = unsafe { FS_ARENA };
    let Some(ptr) = arena_ptr else {
        log::error!("resolve_fs: FS_ARENA is None");
        return None;
    };
    let Some(arena) = (unsafe { ptr.as_ref() }) else {
        log::error!("resolve_fs: FS_ARENA pointer is null");
        return None;
    };
    match arena.get(handle) {
        Some(resource) => {
            log::debug!("resolve_fs: handle {:?} => fs_id={}", handle, resource.fs_id);
            Some(resource.fs_id)
        }
        None => {
            log::error!("resolve_fs: handle {:?} not found in arena", handle);
            None
        }
    }
}

/// Resolve a Folder DynHandle to the FolderResource's (fs_id, dir) by looking up the arena.
pub fn resolve_folder(handle: ipc::RawHandle) -> Option<(u8, *mut lfs_dir_t)> {
    let arena = unsafe { FOLDER_ARENA?.as_ref()? };
    let resource = arena.get(handle)?;
    // Return a raw pointer to the dir — caller must ensure single-threaded access.
    Some((resource.fs_id, &resource.dir as *const lfs_dir_t as *mut lfs_dir_t))
}

impl FsTable {
    /// Allocate a slot and configure littlefs for the given storage, but do NOT
    /// call `lfs_mount`. Returns the slot index (fs_id).
    fn allocate(&mut self, storage: StorageDyn) -> Result<u8, sysmodule_fs_api::FileSystemError> {
        let idx = self
            .slots
            .iter()
            .position(|s| s.is_none())
            .ok_or_else(|| {
                log::error!("allocate: no free slots");
                sysmodule_fs_api::FileSystemError::TooManyFilesystems
            })?;
        log::info!("allocate: using slot {}", idx);

        let block_count = storage
            .block_count()
            .map_err(|_| {
                log::error!("allocate: block_count() failed");
                sysmodule_fs_api::FileSystemError::StorageError
            })?;
        log::info!("allocate: {} blocks", block_count);

        let mut fs = MountedFs {
            lfs: UnsafeCell::new(unsafe { MaybeUninit::zeroed().assume_init() }),
            config: unsafe { MaybeUninit::zeroed().assume_init() },
            storage,
            read_cache: [0u8; CACHE_SIZE],
            prog_cache: [0u8; CACHE_SIZE],
            lookahead_buf: [0u8; LOOKAHEAD_SIZE],
            block_count,
            mounted: false,
        };

        fs.config.read = Some(lfs_read_cb);
        fs.config.prog = Some(lfs_prog_cb);
        fs.config.erase = Some(lfs_erase_cb);
        fs.config.sync = Some(lfs_sync_cb);
        fs.config.read_size = READ_SIZE;
        fs.config.prog_size = PROG_SIZE;
        fs.config.block_size = BLOCK_SIZE;
        fs.config.block_count = block_count;
        fs.config.block_cycles = -1; // disable wear leveling (SD cards)
        fs.config.cache_size = CACHE_SIZE as u32;
        fs.config.lookahead_size = LOOKAHEAD_SIZE as u32;
        fs.config.name_max = 31;

        self.slots[idx] = Some(fs);
        let slot = self.slots[idx].as_mut().unwrap();

        // Now that the struct is at its final address, wire up the raw pointers.
        slot.config.context = slot as *mut MountedFs as *mut c_void;
        slot.config.read_buffer = slot.read_cache.as_mut_ptr() as *mut c_void;
        slot.config.prog_buffer = slot.prog_cache.as_mut_ptr() as *mut c_void;
        slot.config.lookahead_buffer = slot.lookahead_buf.as_mut_ptr() as *mut c_void;

        Ok(idx as u8)
    }

    /// Find a free slot and mount a filesystem on the given storage device.
    /// Returns the slot index (fs_id) on success.
    pub fn mount(&mut self, storage: StorageDyn) -> Result<u8, sysmodule_fs_api::FileSystemError> {
        let idx = self.allocate(storage)?;
        let slot = self.slots[idx as usize].as_mut().unwrap();

        log::info!("mount: calling lfs_mount for slot {}", idx);
        let rc = unsafe { lfs_mount(slot.lfs.get(), &slot.config) };
        if rc != 0 {
            log::error!("mount: lfs_mount failed rc={}", rc);
            self.slots[idx as usize] = None;
            return Err(sysmodule_fs_api::FileSystemError::CorruptFilesystem);
        }
        slot.mounted = true;
        log::info!("mount: slot {} mounted", idx);

        Ok(idx)
    }

    /// Allocate a slot, format the storage, then mount.
    pub fn format(&mut self, storage: StorageDyn) -> Result<u8, sysmodule_fs_api::FileSystemError> {
        let idx = self.allocate(storage)?;
        let slot = self.slots[idx as usize].as_mut().unwrap();

        log::info!("format: calling lfs_format for slot {}", idx);
        let rc = unsafe { lfs_format(slot.lfs.get(), &slot.config) };
        if rc != 0 {
            log::error!("format: lfs_format failed rc={}", rc);
            self.slots[idx as usize] = None;
            return Err(sysmodule_fs_api::FileSystemError::StorageError);
        }

        log::info!("format: calling lfs_mount for slot {}", idx);
        let rc = unsafe { lfs_mount(slot.lfs.get(), &slot.config) };
        if rc != 0 {
            log::error!("format: lfs_mount after format failed rc={}", rc);
            self.slots[idx as usize] = None;
            return Err(sysmodule_fs_api::FileSystemError::StorageError);
        }
        slot.mounted = true;
        log::info!("format: slot {} formatted and mounted", idx);
        Ok(idx)
    }

    /// Get a mounted filesystem by index.
    pub fn get(&self, fs_id: u8) -> Option<&MountedFs> {
        self.slots.get(fs_id as usize).and_then(|s| s.as_ref())
    }

    /// Unmount and free a filesystem slot.
    /// Calls `lfs_unmount` if mounted, then drops the `MountedFs` (which
    /// releases the underlying `StorageDyn` handle).
    pub fn unmount(&mut self, fs_id: u8) {
        if let Some(slot) = self.slots.get_mut(fs_id as usize) {
            if let Some(fs) = slot.as_mut() {
                if fs.mounted {
                    log::info!("unmount: unmounting slot {}", fs_id);
                    fs.mounted = false;
                }
            }
            // Drop the MountedFs — releases StorageDyn (sdmmc handle etc.)
            *slot = None;
        }
    }
}

impl MountedFs {
    /// Get a raw pointer to the lfs_t for passing to littlefs functions.
    pub fn lfs_ptr(&self) -> *mut lfs_t {
        self.lfs.get()
    }
}

// ---------------------------------------------------------------------------
// Deferred-unlink tracking
// ---------------------------------------------------------------------------

const MAX_UNLINK: usize = 16;

struct UnlinkEntry {
    fs_id: u8,
    path: [u8; 64],
    open_count: u8,
    unlinked: bool,
    occupied: bool,
}

static mut UNLINK_TABLE: [UnlinkEntry; MAX_UNLINK] = {
    const EMPTY: UnlinkEntry = UnlinkEntry {
        fs_id: 0,
        path: [0; 64],
        open_count: 0,
        unlinked: false,
        occupied: false,
    };
    [EMPTY; MAX_UNLINK]
};

fn unlink_table() -> &'static mut [UnlinkEntry; MAX_UNLINK] {
    unsafe { &mut *core::ptr::addr_of_mut!(UNLINK_TABLE) }
}

/// Called when a file/dir is opened. Increments the open count for this path.
pub fn track_open(fs_id: u8, path: &[u8; 64]) {
    let tbl = unlink_table();
    // Find existing entry for this (fs_id, path).
    if let Some(e) = tbl.iter_mut().find(|e| e.occupied && e.fs_id == fs_id && e.path == *path) {
        e.open_count = e.open_count.saturating_add(1);
        return;
    }
    // Allocate a new entry.
    if let Some(e) = tbl.iter_mut().find(|e| !e.occupied) {
        e.fs_id = fs_id;
        e.path = *path;
        e.open_count = 1;
        e.unlinked = false;
        e.occupied = true;
    }
}

/// Mark a path for deferred deletion.
pub fn track_unlink(fs_id: u8, path: &[u8; 64]) {
    let tbl = unlink_table();
    if let Some(e) = tbl.iter_mut().find(|e| e.occupied && e.fs_id == fs_id && e.path == *path) {
        e.unlinked = true;
    }
}

/// Called when a file/dir is closed. Decrements the open count.
/// If the count reaches zero and the path was unlinked, performs `lfs_remove`
/// and frees the tracking entry. Returns true if the entry was removed from disk.
pub fn track_close(fs_id: u8, path: &[u8; 64]) {
    let tbl = unlink_table();
    let Some(e) = tbl.iter_mut().find(|e| e.occupied && e.fs_id == fs_id && e.path == *path) else {
        return;
    };
    e.open_count = e.open_count.saturating_sub(1);
    if e.open_count == 0 && e.unlinked {
        // Last handle closed and path was unlinked — remove from disk.
        let fs_tbl = unsafe { table() };
        if let Some(fs) = fs_tbl.get(fs_id) {
            unsafe {
                lfs_remove(
                    fs.lfs_ptr(),
                    e.path.as_ptr() as *const core::ffi::c_char,
                );
            }
        }
        e.occupied = false;
    }
    if e.open_count == 0 && !e.unlinked {
        // Nobody has it open and it wasn't unlinked — free the tracking slot.
        e.occupied = false;
    }
}

// ---------------------------------------------------------------------------
// littlefs C callbacks — bridge to IPC storage
// ---------------------------------------------------------------------------

/// Recover the `MountedFs` from the config context pointer.
unsafe fn ctx(c: *const lfs_config) -> &'static mut MountedFs {
    unsafe { &mut *((*c).context as *mut MountedFs) }
}

unsafe extern "C" fn lfs_read_cb(
    c: *const lfs_config,
    block: lfs_block_t,
    off: lfs_off_t,
    buffer: *mut c_void,
    size: lfs_size_t,
) -> c_int {
    let fs = unsafe { ctx(c) };
    let buf = unsafe { core::slice::from_raw_parts_mut(buffer as *mut u8, size as usize) };

    let byte_start = block * BLOCK_SIZE + off;
    let mut offset = 0usize;
    while offset < size as usize {
        let phys_block = (byte_start + offset as u32) / 512;
        let mut sector = [0u8; 512];
        if fs.storage.read_block(phys_block, &mut sector).is_err() {
            return -5; // LFS_ERR_IO
        }
        let intra = ((byte_start + offset as u32) % 512) as usize;
        let avail = 512 - intra;
        let to_copy = avail.min(size as usize - offset);
        buf[offset..offset + to_copy].copy_from_slice(&sector[intra..intra + to_copy]);
        offset += to_copy;
    }
    0
}

unsafe extern "C" fn lfs_prog_cb(
    c: *const lfs_config,
    block: lfs_block_t,
    off: lfs_off_t,
    buffer: *const c_void,
    size: lfs_size_t,
) -> c_int {
    let fs = unsafe { ctx(c) };
    let data = unsafe { core::slice::from_raw_parts(buffer as *const u8, size as usize) };

    let byte_start = block * BLOCK_SIZE + off;
    let mut offset = 0usize;
    while offset < size as usize {
        let phys_block = (byte_start + offset as u32) / 512;
        let intra = ((byte_start + offset as u32) % 512) as usize;

        if intra == 0 && (size as usize - offset) >= 512 {
            if fs.storage.write_block(phys_block, &data[offset..offset + 512]).is_err() {
                return -5;
            }
            offset += 512;
        } else {
            let mut sector = [0u8; 512];
            if fs.storage.read_block(phys_block, &mut sector).is_err() {
                return -5;
            }
            let avail = 512 - intra;
            let to_copy = avail.min(size as usize - offset);
            sector[intra..intra + to_copy].copy_from_slice(&data[offset..offset + to_copy]);
            if fs.storage.write_block(phys_block, &sector).is_err() {
                return -5;
            }
            offset += to_copy;
        }
    }
    0
}

unsafe extern "C" fn lfs_erase_cb(
    _c: *const lfs_config,
    _block: lfs_block_t,
) -> c_int {
    0
}

unsafe extern "C" fn lfs_sync_cb(_c: *const lfs_config) -> c_int {
    0
}
