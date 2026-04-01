#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use hubris_task_slots::SLOTS;
use once_cell::OnceCell;
use rcard_log::{OptionExt, ResultExt};
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

#[derive(Debug)]
pub struct PartitionConfig {
    pub device: &'static str,
    pub name: &'static str,
    pub offset_bytes: u64,
    pub size_bytes: u64,
    pub erase_size: u64,
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

fn is_managed(name: &str) -> bool {
    MANAGED_PARTITIONS.iter().any(|&m| m == name)
}

// ── Shared MPI handle ──────────────────────────────────────────────

static MPI: OnceCell<Mpi> = OnceCell::new();

fn mpi() -> &'static Mpi {
    MPI.get().log_expect("MPI not initialized")
}

// ── Lease forwarding helpers ───────────────────────────────────────

/// Read from MPI into a caller's write-lease via intermediate buffer.
fn mpi_read_to_lease(
    address: u32,
    lease: &ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
) -> Result<(), StorageError> {
    let len = lease.len();
    let mut tmp = [0u8; 256];
    let mut offset = 0;
    while offset < len {
        let chunk = (len - offset).min(256);
        mpi()
            .read(address + offset as u32, &mut tmp[..chunk])
            .unwrap_or(());
        for (i, &byte) in tmp[..chunk].iter().enumerate() {
            let _ = lease.write(offset + i, byte);
        }
        offset += chunk;
    }
    Ok(())
}

/// Write from a caller's read-lease to MPI via intermediate buffer.
fn mpi_program_from_lease(
    address: u32,
    lease: &ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
) -> Result<(), StorageError> {
    let len = lease.len();
    let mut tmp = [0u8; 256];
    let mut offset = 0;
    while offset < len {
        let chunk = (len - offset).min(256);
        for (i, byte) in tmp[..chunk].iter_mut().enumerate() {
            *byte = lease.read(offset + i).unwrap_or(0);
        }
        mpi()
            .write(address + offset as u32, &tmp[..chunk])
            .unwrap_or(());
        offset += chunk;
    }
    Ok(())
}

// ── Partition resource implementation ───────────────────────────────

struct PartitionResource {
    index: usize,
    offset_bytes: u32,
    size_bytes: u32,
    erase_size: u32,
}

impl Partition for PartitionResource {
    fn acquire(meta: ipc::Meta, name: [u8; 16]) -> Result<Self, AcquireError> {
        let idx = find_partition(&name).ok_or(AcquireError::NotFound)?;
        let config = &PARTITIONS[idx];

        let caller = meta.sender.task_index();
        let is_fs_task = caller == SLOTS.sysmodule_fs.task_index();

        if is_managed(config.name) {
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
            erase_size: config.erase_size as u32,
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
        // Require erase-aligned offset and length
        if offset % self.erase_size != 0 || len % self.erase_size != 0 {
            return Err(StorageError::alignment());
        }
        // Erase then program
        let abs = self.offset_bytes + offset;
        if mpi()
            .erase(abs, len)
            .unwrap_or(Err(EraseError::InvalidAddressAlignment))
            .is_err()
        {
            return Err(StorageError::device(0xFFFF));
        }
        mpi_program_from_lease(abs, &buf)
    }

    fn erase(&mut self, _meta: ipc::Meta, offset: u32, len: u32) -> Result<(), StorageError> {
        if offset.saturating_add(len) > self.size_bytes {
            return Err(StorageError::out_of_range());
        }
        if offset % self.erase_size != 0 || len % self.erase_size != 0 {
            return Err(StorageError::alignment());
        }
        if mpi()
            .erase(self.offset_bytes + offset, len)
            .unwrap_or(Err(EraseError::InvalidAddressAlignment))
            .is_err()
        {
            return Err(StorageError::device(0xFFFF));
        }
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
        Geometry {
            total_size: self.size_bytes,
            erase_size: self.erase_size,
            program_size: 256,
            read_size: 1,
        }
    }
}

impl Drop for PartitionResource {
    fn drop(&mut self) {
        ACQUIRED[self.index].store(false, Ordering::Release);
    }
}

// ── Entry point ─────────────────────────────────────────────────────

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");

    // Open MPI instance 2 (external NOR flash: GD25Q256EWIGR)
    let flash = Mpi::open(
        2,
        MpiConfig {
            prescaler: 2,
            addr_size: AddrSize::ThreeBytes,
            imode: LineMode::Single,
            admode: LineMode::Single,
            dmode: LineMode::Single,
            read_dummy_cycles: 0,
            clock_polarity: ClockPolarity::Normal,
        },
    )
    .log_unwrap()
    .log_expect("storage: failed to open MPI");
    MPI.set(flash).ok();

    ipc::server! {
        Partition: PartitionResource,
    }
}
