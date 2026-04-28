#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use generated::slots::SLOTS;
use once_cell::OnceCell;
use rcard_log::{error, warn, OptionExt, ResultExt};
use storage_api::{Geometry, StorageError};
use sysmodule_mpi_api::*;
use sysmodule_storage_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup Mpi);
sysmodule_mpi_api::bind_mpi!(Mpi = SLOTS.sysmodule_mpi);

// ── Build-time partition config ─────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum PartitionFormat {
    Boot,
    Raw,
    LittleFs,
    RingBuffer,
}

/// Cached flash geometry sourced from BFPT at startup. Populated once
/// inside `main()` via `Mpi::with_sfdp`; serves every later
/// `Partition::geometry()` call without an extra IPC round trip.
static FLASH_GEOMETRY: OnceCell<CachedGeometry> = OnceCell::new();

#[derive(Debug, Clone, Copy)]
struct CachedGeometry {
    /// Smallest erase granularity advertised in BFPT (typically 4 KiB).
    erase_size: u32,
    /// Page program size from BFPT rev-B+ (DWORD 11), or 256 if the
    /// chip doesn't advertise it (safe default across all SPI NOR).
    program_size: u32,
    /// SPI NOR is byte-addressable on reads.
    read_size: u32,
}

impl CachedGeometry {
    fn from_bfpt(bfpt: &sysmodule_mpi_api::sfdp::Bfpt<'_>) -> Self {
        // Smallest advertised erase type — in practice the 4K sector
        // erase, because the driver prefers to subdivide erases into
        // the smallest granularity when alignment demands it. If BFPT
        // advertises none (very old chips), assume 4 KiB.
        let erase_size = bfpt
            .erase_types()
            .iter()
            .flatten()
            .map(|e| e.size_bytes)
            .min()
            .unwrap_or(4096);

        Self {
            erase_size,
            program_size: bfpt.page_size().unwrap_or(256),
            read_size: 1,
        }
    }
}

#[derive(Debug)]
pub struct PartitionConfig {
    pub device: &'static str,
    pub name: &'static str,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    /// `rcard_places::PART_*` bits. Mirrors the flags the host writes
    /// into places.bin's partition table so the two stay in sync.
    pub flags: u32,
    pub format: PartitionFormat,
}

#[derive(Debug)]
pub struct FilesystemMap {
    pub filesystem: &'static str,
    pub name: &'static str,
    pub source_device: &'static str,
    pub source_partition: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/partitions.rs"));

// ── Runtime partition state ─────────────────────────────────────────

const MAX_PARTITIONS: usize = 16;

static ACQUIRED: [AtomicBool; MAX_PARTITIONS] = {
    const FALSE: AtomicBool = AtomicBool::new(false);
    [FALSE; MAX_PARTITIONS]
};

fn find_partition(name: &[u8; 16]) -> Option<usize> {
    let name_len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    let name_str = core::str::from_utf8(&name[..name_len]).ok()?;
    PARTITIONS.iter().position(|p| p.name == name_str)
}

fn is_managed(config: &PartitionConfig) -> bool {
    config.flags & rcard_places::PART_MANAGED != 0
}

// ── Shared MPI handle ──────────────────────────────────────────────

static MPI: OnceCell<Mpi> = OnceCell::new();

fn mpi() -> &'static Mpi {
    MPI.get().log_expect("MPI not initialized")
}

// ── Lease forwarding helpers ───────────────────────────────────────

static mut MPI_TMP: [u8; 256] = [0u8; 256];

/// Total attempts (not retries) for an MPI IPC operation before giving
/// up. An IPC-layer error means the MPI sysmodule died mid-call and got
/// restarted by the supervisor — the partially-done work is gone, so we
/// restart from offset 0. Three attempts buys us one clean death + one
/// flaky restart without masking a genuine persistent fault.
const MPI_IPC_MAX_ATTEMPTS: u32 = 3;

/// Invoke a single MPI IPC call with retry-on-IPC-failure semantics.
/// On `Err(_)` (IPC layer — sysmodule died) we warn and retry up to
/// [`MPI_IPC_MAX_ATTEMPTS`] times. On `Ok(Err(_))` (operation layer —
/// bad address, WEL-not-latched, hardware timeout) we return immediately,
/// those aren't transient.
///
/// For chunked operations where a partial multi-chunk result must be
/// discarded and restarted, inline the retry loop instead — see
/// [`mpi_read_to_lease`] / [`mpi_program_from_lease`].
fn mpi_retry_single<T, OE, IE>(
    mut op: impl FnMut() -> Result<Result<T, OE>, IE>,
    context: &str,
    address: u32,
) -> Result<T, StorageError> {
    for attempt in 0..MPI_IPC_MAX_ATTEMPTS {
        match op() {
            Ok(Ok(v)) => return Ok(v),
            Ok(Err(_)) => return Err(StorageError::device(0xFFFE)),
            Err(_) => {
                warn!(
                    "MPI {} IPC failure at {} (attempt {}/{}), retrying",
                    context,
                    address,
                    attempt + 1,
                    MPI_IPC_MAX_ATTEMPTS,
                );
            }
        }
    }
    error!(
        "MPI {} failed at 0x{:08x} after {} IPC attempts",
        context, address, MPI_IPC_MAX_ATTEMPTS,
    );
    Err(StorageError::device(0xFFFE))
}

/// Read from MPI into a caller's write-lease. Uses bulk `write_range`
/// rather than per-byte `write` — the per-byte path issues one syscall
/// per byte, and (per the comment in `sysmodule_usb`) is "corruption-
/// prone" in addition to being slow.
///
/// If the MPI sysmodule dies mid-read (IPC-layer error), the whole read
/// is restarted from offset 0 up to [`MPI_IPC_MAX_ATTEMPTS`] times. An
/// operation-layer error (MpiOperationError — bad address, WEL latch,
/// timeout) is returned immediately: those aren't transient.
fn mpi_read_to_lease(
    address: u32,
    lease: &ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
) -> Result<(), StorageError> {
    let t0 = userlib::sys_get_timer().now;
    let len = lease.len();
    let tmp = unsafe { &mut *(&raw mut MPI_TMP) };

    'attempt: for attempt in 0..MPI_IPC_MAX_ATTEMPTS {
        let mut offset = 0;
        while offset < len {
            let chunk = (len - offset).min(256);
            match mpi().read(address + offset as u32, &mut tmp[..chunk]) {
                Err(_) => {
                    warn!(
                        "MPI read IPC failure at {} (attempt {}/{}), retrying from start",
                        address,
                        attempt + 1,
                        MPI_IPC_MAX_ATTEMPTS
                    );
                    continue 'attempt;
                }
                Ok(Err(_)) => {
                    return Err(StorageError::device(0xFFFE));
                }
                Ok(Ok(())) => {
                    lease
                        .write_range(offset, &tmp[..chunk])
                        .ok_or_else(|| StorageError::device(0xFFFD))?;
                    offset += chunk;
                }
            }
        }
        let elapsed = userlib::sys_get_timer().now - t0;
        if elapsed > 100 {
            warn!(
                "mpi_read_to_lease: {}ms for {} bytes at 0x{:08x}",
                elapsed, len, address
            );
        }
        return Ok(());
    }

    error!(
        "MPI read failed at 0x{:08x} after {} IPC attempts",
        address, MPI_IPC_MAX_ATTEMPTS
    );
    Err(StorageError::device(0xFFFE))
}

/// Write from a caller's read-lease to MPI. Uses bulk `read_range` —
/// see `mpi_read_to_lease` for why per-byte is bad.
///
/// Same IPC-retry policy as [`mpi_read_to_lease`]: on MPI sysmodule
/// death we restart the whole multi-chunk program from offset 0. This
/// is safe over an erased region because NOR page-program is idempotent
/// (1-bits only ever clear to 0); re-programming the first N chunks
/// writes the same bytes back.
fn mpi_program_from_lease(
    address: u32,
    lease: &ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
) -> Result<(), StorageError> {
    let len = lease.len();
    let tmp = unsafe { &mut *(&raw mut MPI_TMP) };

    'attempt: for attempt in 0..MPI_IPC_MAX_ATTEMPTS {
        let mut offset = 0;
        while offset < len {
            let chunk = (len - offset).min(256);
            lease
                .read_range(offset, &mut tmp[..chunk])
                .ok_or_else(|| StorageError::device(0xFFFD))?;
            match mpi().write(address + offset as u32, &tmp[..chunk]) {
                Err(_) => {
                    warn!(
                        "MPI write IPC failure at {} (attempt {}/{}), retrying from start",
                        address,
                        attempt + 1,
                        MPI_IPC_MAX_ATTEMPTS
                    );
                    continue 'attempt;
                }
                Ok(Err(_)) => {
                    return Err(StorageError::device(0xFFFE));
                }
                Ok(Ok(())) => {
                    offset += chunk;
                }
            }
        }
        return Ok(());
    }

    error!(
        "MPI write failed at 0x{:08x} after {} IPC attempts",
        address, MPI_IPC_MAX_ATTEMPTS
    );
    Err(StorageError::device(0xFFFE))
}

// ── Partition resource implementation ───────────────────────────────

struct PartitionResource {
    index: usize,
    offset_bytes: u32,
    size_bytes: u32,
}

impl Partition for PartitionResource {
    fn acquire(meta: ipc::Meta, name: [u8; 16]) -> Result<Self, AcquireError> {
        let idx = find_partition(&name).ok_or(AcquireError::NotFound)?;
        let config = &PARTITIONS[idx];

        let caller = meta.sender.task_index();
        let is_fs_task = generated::peers::PEERS
            .sysmodule_fs
            .map_or(false, |id| caller == id.task_index());

        if is_managed(config) {
            if !is_fs_task {
                return Err(AcquireError::ManagedByFilesystem);
            }
        } else if !is_partition_allowed(config.name, caller) {
            return Err(AcquireError::NotAllowed);
        }

        if ACQUIRED[idx].swap(true, Ordering::Acquire) {
            return Err(AcquireError::InUse);
        }

        Ok(PartitionResource {
            index: idx,
            offset_bytes: config.offset_bytes as u32,
            size_bytes: config.size_bytes as u32,
        })
    }

    fn read(
        &mut self,
        _meta: ipc::Meta,
        offset: u32,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<(), StorageError> {
        let len = buf.len() as u32;
        if offset.saturating_add(len) > self.size_bytes {
            return Err(StorageError::out_of_range());
        }
        mpi_read_to_lease(self.offset_bytes + offset, &buf)
    }

    fn write(
        &mut self,
        _meta: ipc::Meta,
        offset: u32,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<(), StorageError> {
        let len = buf.len() as u32;
        if offset.saturating_add(len) > self.size_bytes {
            return Err(StorageError::out_of_range());
        }
        // Require erase-aligned offset and length.
        let erase = FLASH_GEOMETRY.get().log_expect("FLASH_GEOMETRY").erase_size;
        if offset % erase != 0 || len % erase != 0 {
            return Err(StorageError::alignment());
        }
        // Erase then program
        let abs = self.offset_bytes + offset;
        mpi_retry_single(|| mpi().erase(abs, len), "erase", abs)?;
        mpi_program_from_lease(abs, &buf)
    }

    fn erase(&mut self, _meta: ipc::Meta, offset: u32, len: u32) -> Result<(), StorageError> {
        if offset.saturating_add(len) > self.size_bytes {
            return Err(StorageError::out_of_range());
        }
        let erase = FLASH_GEOMETRY.get().log_expect("FLASH_GEOMETRY").erase_size;
        if offset % erase != 0 || len % erase != 0 {
            return Err(StorageError::alignment());
        }
        let abs = self.offset_bytes + offset;
        mpi_retry_single(|| mpi().erase(abs, len), "erase", abs)?;
        Ok(())
    }

    fn program(
        &mut self,
        _meta: ipc::Meta,
        offset: u32,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<(), StorageError> {
        let len = buf.len() as u32;
        if offset.saturating_add(len) > self.size_bytes {
            return Err(StorageError::out_of_range());
        }
        mpi_program_from_lease(self.offset_bytes + offset, &buf)
    }

    fn geometry(&mut self, _meta: ipc::Meta) -> Geometry {
        let cached = FLASH_GEOMETRY.get().log_expect("FLASH_GEOMETRY");
        Geometry {
            total_size: self.size_bytes,
            erase_size: cached.erase_size,
            program_size: cached.program_size,
            read_size: cached.read_size,
        }
    }
}

impl Drop for PartitionResource {
    fn drop(&mut self) {
        ACQUIRED[self.index].store(false, Ordering::Release);
    }
}

// ── FlashLayout — read the installed places.bin partition table ─────

/// CPU-space base of the mpi2 mapping. Every address the ftab stores is
/// in this address space; subtract to get an mpi-relative offset.
const MPI2_MAPPING_BASE: u32 = 0x1200_0000;

/// SiFli sec_configuration magic ('SECF', little-endian).
const SEC_CONFIG_MAGIC: u32 = 0x5345_4346;

/// sec_configuration.ftab starts at this byte offset within sec_config.
const SECFG_FTAB_OFFSET: u32 = 4;

/// Each ftab entry is 16 bytes: base, size, xip_base, flags (all u32).
const FTAB_ENTRY_SIZE: u32 = 16;

/// Our-own-use ftab slot that `tfw::pack` writes containing
/// `(places_base, places_size)`. Mirrors `FTAB_PLACES_SLOT` in pack.rs.
const FTAB_PLACES_SLOT: u32 = 14;

/// places.bin footer magic 'PLCB' — last 4 bytes of the image.
const PLACES_MAGIC: u32 = 0x504C_4342;

/// Size of the places.bin trailing footer.
const PLACES_FOOTER_SIZE: u32 = 24;

/// Size of one partition-table entry in places.bin.
const PLACES_PARTITION_SIZE: u32 = 16;

fn mpi_read_u32(address: u32) -> Result<u32, LayoutError> {
    let mut buf = [0u8; 4];
    mpi_retry_single(|| mpi().read(address, &mut buf), "layout-read", address)
        .map_err(|_| LayoutError::ReadFailure)?;
    Ok(u32::from_le_bytes(buf))
}

/// Locate the on-flash `places.bin` via the sec_config's pack-written
/// slot at `ftab[FTAB_PLACES_SLOT]`. Returns `(mpi_base, size)`, or
/// `Unpartitioned` if the ftab is missing / that slot is erased (older
/// firmware that predates this convention, or blank flash).
fn locate_places() -> Result<(u32, u32), LayoutError> {
    // sec_config lives at mpi offset 0 by construction.
    let magic = mpi_read_u32(0)?;
    if magic != SEC_CONFIG_MAGIC {
        return Err(LayoutError::Unpartitioned);
    }

    let entry = SECFG_FTAB_OFFSET + FTAB_PLACES_SLOT * FTAB_ENTRY_SIZE;
    let cpu_base = mpi_read_u32(entry)?;
    let size = mpi_read_u32(entry + 4)?;

    // Erased (0xFFFFFFFF) or plainly nonsensical → treat as unpartitioned.
    if cpu_base == u32::MAX || size == u32::MAX || size == 0 {
        return Err(LayoutError::Unpartitioned);
    }
    if cpu_base < MPI2_MAPPING_BASE {
        return Err(LayoutError::Unpartitioned);
    }
    Ok((cpu_base - MPI2_MAPPING_BASE, size))
}

struct FlashLayoutImpl;

impl FlashLayout for FlashLayoutImpl {
    fn get_layout(_meta: ipc::Meta, start: u32) -> Result<Layout, LayoutError> {
        let (places_base, places_size) = locate_places()?;
        if places_size < PLACES_FOOTER_SIZE {
            return Err(LayoutError::Unpartitioned);
        }

        // Footer is the last 24 bytes of places.bin.
        let footer = places_base + places_size - PLACES_FOOTER_SIZE;
        let tables_offset = mpi_read_u32(footer)?;
        let _segment_count = mpi_read_u32(footer + 4)?;
        let partition_count = mpi_read_u32(footer + 8)?;
        let magic = mpi_read_u32(footer + 20)?;
        if magic != PLACES_MAGIC {
            return Err(LayoutError::Unpartitioned);
        }

        if start >= partition_count {
            return Err(LayoutError::OutOfRange);
        }

        let count = core::cmp::min((partition_count - start) as usize, MAX_ENTRIES_PER_CALL);
        let mut entries = [LayoutEntry {
            name_hash: 0,
            offset: 0,
            size: 0,
            flags: 0,
        }; MAX_ENTRIES_PER_CALL];

        for i in 0..count {
            let part = places_base + tables_offset + (start + i as u32) * PLACES_PARTITION_SIZE;
            entries[i] = LayoutEntry {
                name_hash: mpi_read_u32(part)?,
                offset: mpi_read_u32(part + 4)?,
                size: mpi_read_u32(part + 8)?,
                flags: mpi_read_u32(part + 12)?,
            };
        }

        Ok(Layout {
            total: partition_count,
            start,
            count: count as u32,
            entries,
        })
    }
}

// ── Entry point ─────────────────────────────────────────────────────

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");

    // Open MPI instance 2 (external NOR flash). Addressing width,
    // fast-read opcodes, erase types, and page size are all derived
    // from SFDP inside the MPI driver — no hand-tuning needed here.
    let flash = Mpi::open(
        2,
        MpiConfig {
            prescaler: 2,
            clock_polarity: ClockPolarity::Inverted,
            preferred_mode: ModePreference::Fastest,
        },
    )
    .log_unwrap()
    .log_expect("storage: failed to open MPI");
    MPI.set(flash).ok();

    // Cache SFDP-derived geometry once at startup so
    // `Partition::geometry()` and write-alignment checks don't hit IPC
    // on every call. `with_sfdp` reads the SFDP global header + all
    // parameter headers + BFPT body under one helper call.
    use sysmodule_mpi_api::sfdp::MpiExt;
    let geometry = mpi()
        .with_sfdp(|sfdp| CachedGeometry::from_bfpt(&sfdp.bfpt))
        .log_unwrap();
    FLASH_GEOMETRY.set(geometry).ok();

    ipc::server! {
        Partition: PartitionResource,
        FlashLayout: FlashLayoutImpl,
    }
}
