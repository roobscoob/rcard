use std::io::Write;
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::build_metadata::BuildMetadata;
use crate::compile::CompileArtifact;
use crate::config::AppConfig;
use crate::layout::Layout;

// ── ftab constants (SiFli SF32LB52) ──────────────────────────────────────────

const FTAB_SIZE: usize = 20 * 1024; // 20KB partition table
const SEC_CONFIG_MAGIC: u32 = 0x53454346;
const SECFG_FTAB_OFFSET: usize = 4;
const SECFG_IMG_OFFSET: usize = 0x1000;
const SECFG_RUNIMG_OFFSET: usize = 0x2C00;
const FLASH_TABLE_ENTRY_SIZE: usize = 16;
const IMAGE_HEADER_ENC_SIZE: usize = 512;
const DFU_FLASH_IMG_LCPU: usize = 2;
const DFU_FLASH_IMG_BL: usize = 3;
const CORE_BL: usize = 1;

/// Generate a SiFli ftab binary.
///
/// The ftab points the bootrom at the bootloader stored in the loader
/// flash partition.  `firmware_flash_addr` is the flash address where
/// `places.bin` will be written; the bootloader at that address reads the
/// places header to find the real entry point.
fn build_ftab(ftab_base: u32, firmware_flash_addr: u32, firmware_size: u32) -> Vec<u8> {
    let mut buf = vec![0xFFu8; FTAB_SIZE];

    // Magic
    buf[0..4].copy_from_slice(&SEC_CONFIG_MAGIC.to_le_bytes());

    // ftab[0] = self
    let off = SECFG_FTAB_OFFSET;
    buf[off..off + 4].copy_from_slice(&ftab_base.to_le_bytes());
    buf[off + 4..off + 8].copy_from_slice(&(FTAB_SIZE as u32).to_le_bytes());
    buf[off + 8..off + 16].copy_from_slice(&[0; 8]);

    // ftab[3] = boot target (bootloader partition)
    let off = SECFG_FTAB_OFFSET + 3 * FLASH_TABLE_ENTRY_SIZE;
    buf[off..off + 4].copy_from_slice(&firmware_flash_addr.to_le_bytes());
    buf[off + 4..off + 8].copy_from_slice(&firmware_size.to_le_bytes());
    buf[off + 8..off + 12].copy_from_slice(&firmware_flash_addr.to_le_bytes());
    buf[off + 12..off + 16].copy_from_slice(&0u32.to_le_bytes());

    // imgs[1] = boot image header
    let imgs_bl_idx = DFU_FLASH_IMG_BL - DFU_FLASH_IMG_LCPU;
    let off = SECFG_IMG_OFFSET + imgs_bl_idx * IMAGE_HEADER_ENC_SIZE;
    buf[off..off + 4].copy_from_slice(&firmware_size.to_le_bytes());
    buf[off + 4..off + 6].copy_from_slice(&0u16.to_le_bytes());
    buf[off + 6..off + 8].copy_from_slice(&0u16.to_le_bytes());

    // running_imgs[1]
    let running_img_ptr = ftab_base + SECFG_IMG_OFFSET as u32
        + (imgs_bl_idx as u32) * IMAGE_HEADER_ENC_SIZE as u32;
    let off = SECFG_RUNIMG_OFFSET + CORE_BL * 4;
    buf[off..off + 4].copy_from_slice(&running_img_ptr.to_le_bytes());

    buf
}

/// Build the ftab binary from the config's boot section.
/// The ftab points the bootrom at wherever the layout placed the
/// bootloader's code region — which now sits inside places.bin.
fn build_ftab_from_config(
    config: &AppConfig,
    layout: &Layout,
    bootloader_bin_size: u32,
) -> Option<Vec<u8>> {
    let boot = config.boot.as_ref()?;

    let ftab_place = &boot.ftab;
    let ftab_offset = ftab_place.offset.unwrap_or(0);
    let flash_base = ftab_place.mappings.first()
        .map(|m| m.address)
        .unwrap_or(0x12000000) as u32;
    let ftab_flash_addr = flash_base + ftab_offset as u32;

    let loader_flash_addr = layout
        .placed
        .get(&("bootloader".to_string(), "code".to_string()))?
        .base as u32;

    Some(build_ftab(ftab_flash_addr, loader_flash_addr, bootloader_bin_size))
}

/// Pack the build output into a `.tfw` archive.
///
/// The archive contains:
/// - `places.bin` — the firmware image (partition table + RAM-loadable segments,
///   including the bootloader's XIP segment)
/// - `ftab.bin` — the SiFli partition table (if boot config exists)
/// - `renode_platform.repl` — emulator platform description
/// - `config.json` — full app config for host tools
/// - `elf/kernel`, `elf/bootloader`, `elf/task/*` — ELFs for debugging
/// - `log-metadata.json` — log metadata for structured log decoding
/// - `ipc-metadata.json` — IPC resource / interface / server definitions
pub fn pack(
    config: &AppConfig,
    layout: &Layout,
    artifacts: &[CompileArtifact],
    places_bin: &Path,
    bootloader_size: u32,
    log_metadata: Option<&Path>,
    ipc_metadata: Option<&Path>,
    build_metadata: Option<&BuildMetadata>,
    out_path: &Path,
) -> Result<PathBuf, PackError> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(PackError::Io)?;
    }

    let file = std::fs::File::create(out_path).map_err(PackError::Io)?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default();

    // places.bin
    let places_data = std::fs::read(places_bin).map_err(PackError::Io)?;
    zip.start_file("places.bin", opts).map_err(PackError::Zip)?;
    zip.write_all(&places_data).map_err(PackError::Io)?;

    // ftab.bin (if boot config exists)
    if let Some(ftab) = build_ftab_from_config(config, layout, bootloader_size) {
        zip.start_file("ftab.bin", opts).map_err(PackError::Zip)?;
        zip.write_all(&ftab).map_err(PackError::Io)?;
    }

    // renode_platform.repl
    let repl = crate::renode::generate_repl(config);
    zip.start_file("renode_platform.repl", opts).map_err(PackError::Zip)?;
    zip.write_all(repl.as_bytes()).map_err(PackError::Io)?;

    // config.json
    let config_json = serde_json::to_string_pretty(config)
        .map_err(|e| PackError::Other(format!("serialize config: {e}")))?;
    zip.start_file("config.json", opts).map_err(PackError::Zip)?;
    zip.write_all(config_json.as_bytes()).map_err(PackError::Io)?;

    // Task, kernel, and bootloader ELFs
    for artifact in artifacts {
        let archive_name = match artifact.kind {
            crate::compile::ArtifactKind::Kernel => "elf/kernel".to_string(),
            crate::compile::ArtifactKind::Bootloader => "elf/bootloader".to_string(),
            crate::compile::ArtifactKind::Task => format!("elf/task/{}", artifact.crate_name),
        };
        let data = std::fs::read(&artifact.elf_path).map_err(PackError::Io)?;
        zip.start_file(&archive_name, opts).map_err(PackError::Zip)?;
        zip.write_all(&data).map_err(PackError::Io)?;
    }

    // Log metadata
    if let Some(meta_path) = log_metadata {
        if meta_path.exists() {
            let data = std::fs::read(meta_path).map_err(PackError::Io)?;
            zip.start_file("log-metadata.json", opts).map_err(PackError::Zip)?;
            zip.write_all(&data).map_err(PackError::Io)?;
        }
    }

    // IPC metadata
    if let Some(meta_path) = ipc_metadata {
        if meta_path.exists() {
            let data = std::fs::read(meta_path).map_err(PackError::Io)?;
            zip.start_file("ipc-metadata.json", opts).map_err(PackError::Zip)?;
            zip.write_all(&data).map_err(PackError::Io)?;
        }
    }

    // Build metadata
    if let Some(meta) = build_metadata {
        let json = serde_json::to_string_pretty(meta)
            .map_err(|e| PackError::Other(format!("serialize build metadata: {e}")))?;
        zip.start_file("build-metadata.json", opts).map_err(PackError::Zip)?;
        zip.write_all(json.as_bytes()).map_err(PackError::Io)?;
    }

    zip.finish().map_err(PackError::Zip)?;

    Ok(out_path.to_path_buf())
}

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("pack IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("pack ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("pack error: {0}")]
    Other(String),
}
