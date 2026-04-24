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
use crate::layout::Layout;
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
/// free for app-specific metadata. Stub `get_layout` reads this entry to
/// locate the on-flash places.bin (base + exact byte length) without
/// having to scan for the `'PLCB'` footer magic.
pub const FTAB_PLACES_SLOT: usize = 14;

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
fn build_ftab(
    ftab_base: u32,
    loader_flash_src: u32,
    loader_xip_dest: u32,
    loader_size: u32,
    places_base: u32,
    places_size: u32,
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

    // ftab[FTAB_PLACES_SLOT]: our own pointer to places.bin. Unused by
    // the BOOTROM; consumed by the stub's `FlashLayout::get_layout` to
    // locate the PLCB footer without scanning.
    sec.ftab[FTAB_PLACES_SLOT] = FlashTable {
        base: places_base,
        size: places_size,
        xip_base: 0,
        flags: 0,
    };

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

/// Build the ftab binary from the config's boot section.
///
/// The bootloader is linked into SRAM (its `code` region lives in `bulk`),
/// but its bytes are physically packed inside `places.bin` on flash. To
/// take the BOOTROM's copy-then-jump path we need both addresses:
///
///   loader_xip_dest = layout.placed[bootloader.code].base   (SRAM linker addr)
///   loader_flash_src = places_flash_addr                    (place hosting places.bin)
///                    + place_layouts[bulk].file_offset      (where bulk's blob starts in the file)
///                    + (bootloader_alloc.base - bulk_blob_base)
///                                                            (bootloader's offset within the blob)
fn build_ftab_from_config(
    config: &AppConfig,
    layout: &Layout,
    place_layouts: &BTreeMap<String, PlaceLayout>,
    bootloader_bin_size: u32,
    places_bin_size: u32,
) -> Option<Vec<u8>> {
    let boot = config.boot.as_ref()?;

    let ftab_place = &boot.ftab;
    let ftab_offset = ftab_place.offset.unwrap_or(0);
    let flash_base = ftab_place
        .mappings
        .first()
        .map(|m| m.address)
        .unwrap_or(0x12000000) as u32;
    let ftab_flash_addr = flash_base + ftab_offset as u32;

    // Base of the places-hosting partition. Same mapping resolution as
    // the ftab address — first CPU mapping plus the place's offset.
    let image_place = &boot.image;
    let image_flash_base = image_place
        .mappings
        .first()
        .map(|m| m.address)
        .unwrap_or(0x12000000) as u32;
    let places_flash_addr = image_flash_base + image_place.offset.unwrap_or(0) as u32;

    // SRAM destination: the bootloader's linker base.
    let bootloader_alloc = layout
        .placed
        .get(&("bootloader".to_string(), "code".to_string()))?;
    let loader_xip_dest = bootloader_alloc.base as u32;

    // Flash source: walk back through the link.rs packing to find where
    // the bootloader's bytes physically landed in places.bin.
    let bl_place_name = crate::layout::find_place_name(config, bootloader_alloc.base)?;
    let pl = place_layouts.get(&bl_place_name)?;
    let loader_flash_src = (places_flash_addr as u64
        + pl.file_offset as u64
        + (bootloader_alloc.base - pl.blob_base)) as u32;

    Some(build_ftab(
        ftab_flash_addr,
        loader_flash_src,
        loader_xip_dest,
        bootloader_bin_size,
        places_flash_addr,
        places_bin_size,
    ))
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
    place_layouts: &BTreeMap<String, PlaceLayout>,
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
    let places_bin_size: u32 = places_data
        .len()
        .try_into()
        .map_err(|_| PackError::Other("places.bin size exceeds u32".into()))?;
    zip.start_file("places.bin", opts).map_err(PackError::Zip)?;
    zip.write_all(&places_data).map_err(PackError::Io)?;

    // ftab.bin (if boot config exists)
    if let Some(ftab) =
        build_ftab_from_config(config, layout, place_layouts, bootloader_size, places_bin_size)
    {
        zip.start_file("ftab.bin", opts).map_err(PackError::Zip)?;
        zip.write_all(&ftab).map_err(PackError::Io)?;
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

    // Task, kernel, and bootloader ELFs
    for artifact in artifacts {
        let archive_name = match artifact.kind {
            crate::compile::ArtifactKind::Kernel => "elf/kernel".to_string(),
            crate::compile::ArtifactKind::Bootloader => "elf/bootloader".to_string(),
            crate::compile::ArtifactKind::Task => format!("elf/task/{}", artifact.crate_name),
        };
        let data = std::fs::read(&artifact.elf_path).map_err(PackError::Io)?;
        zip.start_file(&archive_name, opts)
            .map_err(PackError::Zip)?;
        zip.write_all(&data).map_err(PackError::Io)?;
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
