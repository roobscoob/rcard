//! Global shared state for mounted filesystems.
//!
//! All resource implementations (FileSystem, File, Folder) access this shared
//! state through [`with_state`], which provides exclusive `&mut FsState` access
//! via [`GlobalState`] with reentrance detection.

use core::cell::UnsafeCell;
use core::ffi::{c_int, c_void};

use littlefs2_sys::*;
use once_cell::GlobalState;
use storage_api::StorageDyn;

/// Maximum number of simultaneously mounted filesystems.
pub const MAX_FS: usize = 4;
/// Maximum number of simultaneously open files.
pub const MAX_OPEN_FILES: usize = 4;
/// Maximum number of simultaneously open directories.
pub const MAX_OPEN_DIRS: usize = 4;
/// Maximum number of filesystem registry entries.
pub const MAX_REGISTRY_ENTRIES: usize = 8;

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

// ---------------------------------------------------------------------------
// Per-filesystem context
// ---------------------------------------------------------------------------

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

impl MountedFs {
    /// Get a raw pointer to the lfs_t for passing to littlefs functions.
    pub fn lfs_ptr(&self) -> *mut lfs_t {
        self.lfs.get()
    }
}

// ---------------------------------------------------------------------------
// Open file slot
// ---------------------------------------------------------------------------

pub struct OpenFile {
    pub file: lfs_file_t,
    pub file_cfg: lfs_file_config,
    pub file_cache: [u8; 512],
    pub path: [u8; 64],
    pub fs_id: u8,
    pub refcount: u8,
    pub occupied: bool,
    pub unlinked: bool,
    pub lfs_flags: i32,
}

// ---------------------------------------------------------------------------
// Open directory slot
// ---------------------------------------------------------------------------

pub struct OpenDir {
    pub dir: lfs_dir_t,
    pub path: [u8; 64],
    pub fs_id: u8,
    pub refcount: u8,
    pub occupied: bool,
    pub unlinked: bool,
    pub generation: u32,
}

// ---------------------------------------------------------------------------
// Registry entry
// ---------------------------------------------------------------------------

pub struct RegistryEntry {
    pub name: [u8; 16],
    pub fs_id: Option<u8>,
}

// ---------------------------------------------------------------------------
// FsTable
// ---------------------------------------------------------------------------

/// Global table of mounted filesystems.
pub struct FsTable {
    pub slots: [Option<MountedFs>; MAX_FS],
}

impl FsTable {
    const fn new() -> Self {
        Self {
            slots: [const { None }; MAX_FS],
        }
    }

    /// Allocate a slot and configure littlefs for the given storage, but do NOT
    /// call `lfs_mount`. Returns the slot index (fs_id).
    fn allocate(&mut self, storage: StorageDyn) -> Result<u8, sysmodule_fs_api::FileSystemError> {
        let idx = self.slots.iter().position(|s| s.is_none()).ok_or_else(|| {
            log::error!("allocate: no free slots");
            sysmodule_fs_api::FileSystemError::TooManyFilesystems
        })?;
        log::info!("allocate: using slot {}", idx);

        let block_count = storage.block_count().map_err(|_| {
            log::error!("allocate: block_count() failed");
            sysmodule_fs_api::FileSystemError::StorageError
        })?;
        log::info!("allocate: {} blocks", block_count);

        let mut fs = MountedFs {
            lfs: UnsafeCell::new(unsafe { core::mem::zeroed() }),
            config: unsafe { core::mem::zeroed() },
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
        fs.config.block_cycles = 500; // wear leveling (eMMC, ~60k P/E cycles)
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
    pub fn unmount(&mut self, fs_id: u8) {
        if let Some(slot) = self.slots.get_mut(fs_id as usize) {
            if let Some(fs) = slot.as_mut() {
                if fs.mounted {
                    log::info!("unmount: unmounting slot {}", fs_id);
                    unsafe { lfs_unmount(fs.lfs.get()) };
                    fs.mounted = false;
                }
            }
            *slot = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Combined global state
// ---------------------------------------------------------------------------

pub struct FsState {
    pub fs_table: FsTable,
    pub open_files: [OpenFile; MAX_OPEN_FILES],
    pub open_dirs: [OpenDir; MAX_OPEN_DIRS],
    pub registry: [RegistryEntry; MAX_REGISTRY_ENTRIES],
}

impl FsState {
    const fn new() -> Self {
        const EMPTY_FILE: OpenFile = OpenFile {
            file: unsafe { core::mem::zeroed() },
            file_cfg: unsafe { core::mem::zeroed() },
            file_cache: [0u8; 512],
            path: [0u8; 64],
            fs_id: 0,
            refcount: 0,
            occupied: false,
            unlinked: false,
            lfs_flags: 0,
        };
        const EMPTY_DIR: OpenDir = OpenDir {
            dir: unsafe { core::mem::zeroed() },
            path: [0u8; 64],
            fs_id: 0,
            refcount: 0,
            occupied: false,
            unlinked: false,
            generation: 0,
        };
        const EMPTY_REG: RegistryEntry = RegistryEntry {
            name: [0; 16],
            fs_id: None,
        };

        Self {
            fs_table: FsTable::new(),
            open_files: [EMPTY_FILE; MAX_OPEN_FILES],
            open_dirs: [EMPTY_DIR; MAX_OPEN_DIRS],
            registry: [EMPTY_REG; MAX_REGISTRY_ENTRIES],
        }
    }
}

static FS_STATE: GlobalState<FsState> = GlobalState::new(FsState::new());

/// Access the global filesystem state exclusively through a closure.
///
/// Panics if called reentrantly (e.g. from within a littlefs callback).
pub fn with_state<R>(f: impl FnOnce(&mut FsState) -> R) -> R {
    FS_STATE.with(f)
}

// ---------------------------------------------------------------------------
// littlefs C callbacks — bridge to IPC storage
// ---------------------------------------------------------------------------

/// Recover the `MountedFs` from the config context pointer.
///
/// # Safety
/// Must only be called from littlefs callbacks where `c` is a valid
/// `lfs_config` whose `context` field points to a live `MountedFs`.
/// These callbacks run from within a `with_state` closure, so they must
/// NOT call `with_state` again.
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
            if fs
                .storage
                .write_block(phys_block, &data[offset..offset + 512])
                .is_err()
            {
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

unsafe extern "C" fn lfs_erase_cb(_c: *const lfs_config, _block: lfs_block_t) -> c_int {
    0
}

unsafe extern "C" fn lfs_sync_cb(_c: *const lfs_config) -> c_int {
    0
}
