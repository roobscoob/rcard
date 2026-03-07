#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use hubris_task_slots::SLOTS;
use sysmodule_storage_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
sysmodule_log_api::panic_handler!(Log);
sysmodule_sdmmc_api::bind_sdmmc!(Sdmmc = SLOTS.sysmodule_sdmmc);

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
    pub offset_blocks: u64,
    pub size_blocks: u64,
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

// ── Shared SDMMC handle ────────────────────────────────────────────

static mut SDMMC: Option<Sdmmc> = None;

fn sdmmc() -> &'static Sdmmc {
    unsafe { SDMMC.as_ref().expect("SDMMC not initialized") }
}

// ── Partition resource implementation ───────────────────────────────

struct PartitionResource {
    index: usize,
    offset: u32,
    count: u32,
}

impl Partition for PartitionResource {
    fn acquire(meta: ipc::Meta, name: [u8; 16]) -> Result<Self, AcquireError> {
        let idx = find_partition(&name).ok_or(AcquireError::NotFound)?;
        let config = &PARTITIONS[idx];

        let caller = meta.sender.task_index();
        let is_fs_task = caller == SLOTS.sysmodule_fs.task_index();

        if is_managed(config.name) {
            // Managed partitions can only be acquired by the fs task
            if !is_fs_task {
                return Err(AcquireError::ManagedByFilesystem);
            }
        } else {
            // Non-managed partitions require an explicit uses-partition ACL entry
            if !is_partition_allowed(config.name, caller) {
                return Err(AcquireError::NotAllowed);
            }
        }

        if ACQUIRED[idx].swap(true, Ordering::Acquire) {
            return Err(AcquireError::InUse);
        }

        log::info!("partition {:?} acquired (offset={}, size={})",
            config.name, config.offset_blocks, config.size_blocks);

        Ok(PartitionResource {
            index: idx,
            offset: config.offset_blocks as u32,
            count: config.size_blocks as u32,
        })
    }

    fn read_block(
        &mut self,
        _meta: ipc::Meta,
        block: u32,
        buf: idyll_runtime::Leased<idyll_runtime::Write, u8>,
    ) {
        if block >= self.count {
            return;
        }
        let mut tmp = [0u8; 512];
        if sdmmc().read_block(self.offset + block, &mut tmp).is_ok() {
            let len = buf.len().min(512);
            for i in 0..len {
                let _ = buf.write(i, tmp[i]);
            }
        }
    }

    fn write_block(
        &mut self,
        _meta: ipc::Meta,
        block: u32,
        buf: idyll_runtime::Leased<idyll_runtime::Read, u8>,
    ) {
        if block >= self.count {
            return;
        }
        let mut tmp = [0u8; 512];
        let len = buf.len().min(512);
        for i in 0..len {
            tmp[i] = buf.read(i).unwrap_or(0);
        }
        let _ = sdmmc().write_block(self.offset + block, &tmp);
    }

    fn block_count(&mut self, _meta: ipc::Meta) -> u32 {
        self.count
    }
}

impl Drop for PartitionResource {
    fn drop(&mut self) {
        log::info!("partition {:?} released", PARTITIONS[self.index].name);
        ACQUIRED[self.index].store(false, Ordering::Release);
    }
}

// ── Entry point ─────────────────────────────────────────────────────

#[export_name = "main"]
fn main() -> ! {
    sysmodule_log_api::init_logger!(Log);

    // Acquire the raw SDMMC device.
    let sdmmc = Sdmmc::open().unwrap().expect("storage: failed to acquire SDMMC");
    unsafe { SDMMC = Some(sdmmc) };

    log::info!("storage: {} partitions configured", PARTITIONS.len());
    for p in PARTITIONS {
        log::info!("  {:?}: offset={} size={} format={:?}{}",
            p.name, p.offset_blocks, p.size_blocks, p.format,
            if is_managed(p.name) { " [filesystem]" } else { "" });
    }

    ipc::server! {
        Partition: PartitionResource,
    }
}
