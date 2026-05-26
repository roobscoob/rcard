use std::collections::BTreeMap;
use std::io::Write;
use std::mem::{offset_of, size_of};
use std::path::{Path, PathBuf};

use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use crate::build_metadata::BuildMetadata;
use crate::compile::CompileArtifact;
use crate::config::AppConfig;
use crate::link::PlaceLayout;

// ── ftab / sec_configuration (SiFli SF32LB52) ────────────────────────────────
//
// Mirrors `struct sec_configuration` from the SiFli SDK
// (middleware/dfu/dfu.h). The BOOTROM reads `.magic` and
// `.running_imgs[CORE_BL]` to find the next-stage bootloader;
// `dfu_boot_img_in_flash` downstream reads `.ftab[flash_id]` and
// `.imgs[flash_id - 2]` to set up XIP and (optionally) verify/decrypt.
// We leave the crypto fields erased (0xFF) and set flags=0, which tells
// the BOOTROM to take the plaintext path and skip RSA/AES entirely.

const FTAB_SIZE: usize = 20 * 1024; // partition size; struct occupies the first ~11K
const SEC_CONFIG_MAGIC: u32 = 0x53454346; // "SECF"

const DFU_FLASH_PARTITION: usize = 16;
const DFU_FLASH_IMG_LCPU: usize = 2;
const DFU_FLASH_IMG_BL: usize = 3;
/// `DFU_FLASH_IMG_IDX(DFU_FLASH_IMG_BL)` — index into `imgs[]` for the bootloader.
const DFU_FLASH_IMG_IDX_BL: usize = DFU_FLASH_IMG_BL - DFU_FLASH_IMG_LCPU;
const IMG_COUNT: usize = DFU_FLASH_PARTITION - DFU_FLASH_IMG_LCPU;

/// Our-own-use slot within `ftab[]`. The BOOTROM only reads ftab[0] (self)
/// and ftab[DFU_FLASH_IMG_BL] (bootloader), so indices 1, 2, 4..16 are
/// free for app-specific metadata. The bootloader reads both slots to
/// select the best firmware image at boot.
pub const FTAB_PLACES_SLOT_A: usize = 14;
pub const FTAB_PLACES_SLOT_B: usize = 15;

const CORE_MAX: usize = 4;
const CORE_BL: usize = 1;

const DFU_SIG_KEY_SIZE: usize = 294;
const DFU_KEY_SIZE: usize = 32;
const DFU_SIG_SIZE: usize = 256;
const DFU_VERSION_LEN: usize = 20;

#[repr(C)]
#[derive(Copy, Clone, Debug, FromBytes, IntoBytes, Immutable, KnownLayout)]
struct FlashTable {
    base: u32,
    size: u32,
    xip_base: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, FromBytes, IntoBytes, Immutable, KnownLayout)]
struct ImageHeaderEnc {
    length: u32,
    blksize: u16,
    flags: u16,
    key: [u8; DFU_KEY_SIZE],
    sig: [u8; DFU_SIG_SIZE],
    ver: [u8; DFU_VERSION_LEN],
    /// Pads the header out to 512B (matches the flash-erase granularity of
    /// the image-header region).
    _reserved: [u8; 256 - DFU_KEY_SIZE - 8 - DFU_VERSION_LEN],
}

// Pre-imgs sector layout: magic + ftab + pub_key, padded to the 4096-byte
// flash-sector boundary so imgs[] starts fresh.
const SIGKEY_OFFSET: usize = 4 + DFU_FLASH_PARTITION * size_of::<FlashTable>();
const SEC_RESERVED_SIZE: usize = 4096 - (SIGKEY_OFFSET + DFU_SIG_KEY_SIZE);

#[repr(C)]
#[derive(Copy, Clone, FromBytes, IntoBytes, Immutable, KnownLayout)]
struct SecConfiguration {
    magic: u32,
    ftab: [FlashTable; DFU_FLASH_PARTITION],
    sig_pub_key: [u8; DFU_SIG_KEY_SIZE],
    _reserved: [u8; SEC_RESERVED_SIZE],
    imgs: [ImageHeaderEnc; IMG_COUNT],
    /// On-target these are `struct image_header_enc *` (32-bit flash
    /// addresses). Kept as `u32` so the host-side layout matches the target.
    running_imgs: [u32; CORE_MAX],
}

// Anchor the struct layout to the known-good C values. If any of these
// asserts fire, the C header drifted (or something about repr(C) did).
const _: () = assert!(size_of::<FlashTable>() == 16);
const _: () = assert!(size_of::<ImageHeaderEnc>() == 512);
const _: () = assert!(offset_of!(SecConfiguration, ftab) == 4);
const _: () = assert!(offset_of!(SecConfiguration, imgs) == 0x1000);
const _: () = assert!(offset_of!(SecConfiguration, running_imgs) == 0x2C00);
const _: () = assert!(size_of::<SecConfiguration>() == 11280);
const _: () = assert!(FTAB_SIZE >= size_of::<SecConfiguration>());

/// Generate a SiFli ftab binary.
///
/// `loader_flash_src` is the flash byte address of the bootloader bytes
/// (the source for `g_flash_read`); `loader_xip_dest` is the SRAM
/// address where the bootloader is linked and where the BOOTROM should
/// jump after the copy. With `flags=0` and `src != dest in RAM`,
/// `dfu_boot_img_in_flash` takes the copy-then-jump branch.
///
/// Crypto fields are left as erased-flash (0xFF) and image flags=0 —
/// `dfu_boot_img_in_flash` treats that as the plaintext path (no AES, no
/// signature verification).
///
/// Called by the host app at flash time — NOT during the build.
pub fn build_ftab(
    ftab_base: u32,
    loader_flash_src: u32,
    loader_xip_dest: u32,
    loader_size: u32,
    images: &[(usize, u32, u32)], // (ftab_slot, flash_addr, size)
) -> Vec<u8> {
    // Start with "erased flash" everywhere, then overwrite only the fields
    // the BOOTROM actually reads. Matches original semantics of
    // `vec![0xFF; FTAB_SIZE]` plus targeted writes.
    let mut sec = SecConfiguration::new_zeroed();
    sec.as_mut_bytes().fill(0xFF);

    sec.magic = SEC_CONFIG_MAGIC;

    // ftab[0] = self (partition-table partition).
    sec.ftab[0] = FlashTable {
        base: ftab_base,
        size: FTAB_SIZE as u32,
        xip_base: 0,
        flags: 0,
    };

    // ftab[DFU_FLASH_IMG_BL] = bootloader partition.
    // base = flash byte source, xip_base = SRAM destination. BOOTROM does
    // g_flash_read(base, xip_base, imgs[BL].length) then jumps to xip_base.
    sec.ftab[DFU_FLASH_IMG_BL] = FlashTable {
        base: loader_flash_src,
        size: loader_size,
        xip_base: loader_xip_dest,
        flags: 0,
    };

    // Firmware image entries — one per image.
    for &(slot, base, size) in images {
        if slot < DFU_FLASH_PARTITION {
            sec.ftab[slot] = FlashTable {
                base,
                size,
                xip_base: 0,
                flags: 0,
            };
        }
    }

    // imgs[BL]: only length/blksize/flags are meaningful. key/sig/ver stay
    // 0xFF — flags=0 (no DFU_FLAG_ENC) means `dfu_boot_img_in_flash` takes
    // the plaintext branch at the end of the function and never touches them.
    let img = &mut sec.imgs[DFU_FLASH_IMG_IDX_BL];
    img.length = loader_size;
    img.blksize = 0;
    img.flags = 0;

    // running_imgs[CORE_BL] = flash-resident pointer to imgs[BL]. The BOOTROM
    // reverse-computes flash_id from this pointer via:
    //   ((ptr - ftab_base - 0x1000) / sizeof(image_header_enc)) + DFU_FLASH_IMG_LCPU
    // ...which must yield DFU_FLASH_IMG_BL. The const_asserts above guarantee
    // offset_of(imgs) == 0x1000.
    let imgs_offset = offset_of!(SecConfiguration, imgs);
    let running_img_ptr = ftab_base
        + imgs_offset as u32
        + (DFU_FLASH_IMG_IDX_BL as u32) * size_of::<ImageHeaderEnc>() as u32;
    sec.running_imgs[CORE_BL] = running_img_ptr;

    // Copy populated struct into the 20K partition buffer; trailing bytes
    // stay 0xFF (erased).
    let mut buf = vec![0xFFu8; FTAB_SIZE];
    buf[..size_of::<SecConfiguration>()].copy_from_slice(sec.as_bytes());
    buf
}


// ── Flash-time metadata ─────────────────────────────────────────────────────

/// Metadata needed to construct the ftab at flash time. Serialized as
/// `flash-info.json` in the archive. The host app reads this, combines
/// it with device state, and calls `build_ftab()` to produce the binary.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FlashInfo {
    /// Flash address of the boot/ftab partition.
    pub ftab_base: u32,
    /// Bootloader's byte offset within each places.bin file. The flash
    /// source address is `image_flash_addr + bootloader_file_offset`.
    pub bootloader_file_offset: u32,
    /// SRAM address the BOOTROM copies the bootloader to.
    pub bootloader_sram_dest: u32,
    /// Bootloader binary size in bytes.
    pub bootloader_size: u32,
    /// Per-image flash info.
    pub images: Vec<ImageFlashInfo>,
}

/// Per-image flash metadata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageFlashInfo {
    /// Image name (matches archive filename: first = "places.bin",
    /// rest = "places_{name}.bin").
    pub name: String,
    /// Flash address of the partition hosting this image.
    pub flash_addr: u32,
    /// Default ftab slot for this image (14 for A, 15 for B).
    pub ftab_slot: usize,
}

/// Pack the build output into a `.tfw` archive.
///
/// The archive contains:
/// - `places.bin`, `places_{name}.bin` — per-image firmware binaries
/// - `flash-info.json` — metadata for ftab construction at flash time
/// - `renode_platform.repl` — emulator platform description
/// - `config.json` — full app config for host tools
/// - `elf/{image_name}/kernel`, `elf/{image_name}/task/*` — per-image ELFs
/// - `log-metadata.json` — log metadata for structured log decoding
/// - `ipc-metadata.json` — IPC resource / interface / server definitions
///
/// Each entry in `images` is (spec, config, bin_path, place_layouts, artifacts).
/// The ftab is NOT included — it is generated at flash time by the host
/// app, which can merge device state with the new firmware's partition info.
pub fn pack(
    config: &AppConfig,
    images: &[(&crate::config::ImageSpec, &AppConfig, &Path, &BTreeMap<String, PlaceLayout>, &[CompileArtifact])],
    flash_info: Option<&FlashInfo>,
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

    // flash-info.json
    if let Some(fi) = flash_info {
        let json = serde_json::to_string_pretty(fi)
            .map_err(|e| PackError::Other(format!("serialize flash info: {e}")))?;
        zip.start_file("flash-info.json", opts).map_err(PackError::Zip)?;
        zip.write_all(json.as_bytes()).map_err(PackError::Io)?;
    }

    // Write all image binaries.
    for (i, (spec, _img_config, bin_path, _place_layouts, _artifacts)) in images.iter().enumerate() {
        let data = std::fs::read(bin_path).map_err(PackError::Io)?;
        let archive_name = if i == 0 {
            "places.bin".to_string()
        } else {
            format!("places_{}.bin", spec.name)
        };
        zip.start_file(&archive_name, opts).map_err(PackError::Zip)?;
        zip.write_all(&data).map_err(PackError::Io)?;
    }

    // renode_platform.repl
    let repl = crate::renode::generate_repl(config);
    zip.start_file("renode_platform.repl", opts)
        .map_err(PackError::Zip)?;
    zip.write_all(repl.as_bytes()).map_err(PackError::Io)?;

    // config.json
    let config_json = serde_json::to_string_pretty(config)
        .map_err(|e| PackError::Other(format!("serialize config: {e}")))?;
    zip.start_file("config.json", opts)
        .map_err(PackError::Zip)?;
    zip.write_all(config_json.as_bytes())
        .map_err(PackError::Io)?;

    // Per-image ELFs: elf/{image_name}/kernel, elf/{image_name}/task/foo
    for (spec, _img_config, _bin_path, _place_layouts, artifacts) in images {
        for artifact in *artifacts {
            let archive_name = match artifact.kind {
                crate::compile::ArtifactKind::Kernel => {
                    format!("elf/{}/kernel", spec.name)
                }
                crate::compile::ArtifactKind::Bootloader => {
                    format!("elf/{}/bootloader", spec.name)
                }
                crate::compile::ArtifactKind::Task => {
                    format!("elf/{}/task/{}", spec.name, artifact.crate_name)
                }
            };
            let data = std::fs::read(&artifact.elf_path).map_err(PackError::Io)?;
            zip.start_file(&archive_name, opts)
                .map_err(PackError::Zip)?;
            zip.write_all(&data).map_err(PackError::Io)?;
        }
    }

    // Log metadata
    if let Some(meta_path) = log_metadata {
        if meta_path.exists() {
            let data = std::fs::read(meta_path).map_err(PackError::Io)?;
            zip.start_file("log-metadata.json", opts)
                .map_err(PackError::Zip)?;
            zip.write_all(&data).map_err(PackError::Io)?;
        }
    }

    // IPC metadata
    if let Some(meta_path) = ipc_metadata {
        if meta_path.exists() {
            let data = std::fs::read(meta_path).map_err(PackError::Io)?;
            zip.start_file("ipc-metadata.json", opts)
                .map_err(PackError::Zip)?;
            zip.write_all(&data).map_err(PackError::Io)?;
        }
    }

    // Build metadata
    if let Some(meta) = build_metadata {
        let json = serde_json::to_string_pretty(meta)
            .map_err(|e| PackError::Other(format!("serialize build metadata: {e}")))?;
        zip.start_file("build-metadata.json", opts)
            .map_err(PackError::Zip)?;
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
