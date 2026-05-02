#![no_std]
#![no_main]

extern crate alloc;

mod heap;

use alloc::vec::Vec;
use generated::slots::SLOTS;
use ipc::allocation;
use lilla_oxid::graphics::{self, Color, Image, ImageFormat as LillaFormat, Point, Size};
use once_cell::{GlobalState, OnceCell};
use rcard_log::{OptionExt, ResultExt};
use sysmodule_compositor_api::*;
use sysmodule_display_api::DisplayConfiguration;

sysmodule_display_api::bind_display!(Display = SLOTS.sysmodule_display);
sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup Display, Reactor);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

const FRAME_BUFFERS_BYTES: usize = 512 * 1024;
allocation!(FRAME_BUFFERS = @frame_buffers: [u8; FRAME_BUFFERS_BYTES]);

#[global_allocator]
static HEAP: heap::Heap = heap::Heap::new();

const DISPLAY_WIDTH: i32 = 128;
const DISPLAY_HEIGHT: i32 = 64;
const DISPLAY_PAGES: usize = (DISPLAY_HEIGHT / 8) as usize;
const DISPLAY_BUF_SIZE: usize = DISPLAY_WIDTH as usize * DISPLAY_PAGES;

const MAX_FRAME_BUFFERS: usize = 32; // matches FrameBuffer arena_size
const MAX_LAYERS: usize = 32; // matches Layer arena_size
const MAX_IMAGE_DIM: u32 = 4096;

struct FrameBufferEntry {
    id: u32,
    image: Image,
}

struct Registry {
    slots: [Option<FrameBufferEntry>; MAX_FRAME_BUFFERS],
    next_id: u32,
}

impl Registry {
    fn find_image(&self, id: u32) -> Option<&Image> {
        self.slots
            .iter()
            .filter_map(|s| s.as_ref())
            .find(|e| e.id == id)
            .map(|e| &e.image)
    }

    fn contains(&self, id: u32) -> bool {
        self.find_image(id).is_some()
    }
}

struct LayerEntry {
    framebuffer_id: u32,
    x: i16,
    y: i16,
    z: i16,
    /// Reserved for future blend modes (alpha-mask, additive, etc.).
    /// Today only `Replace` is honored.
    _blend: BlendMode,
    visible: bool,
}

struct Layers {
    slots: [Option<LayerEntry>; MAX_LAYERS],
}

struct Scratch {
    frame: Image,
    out: [u8; DISPLAY_BUF_SIZE],
}

static REGISTRY: OnceCell<GlobalState<Registry>> = OnceCell::new();
static LAYERS: OnceCell<GlobalState<Layers>> = OnceCell::new();
static SCRATCH: OnceCell<GlobalState<Scratch>> = OnceCell::new();
static DISPLAY_HANDLE: OnceCell<Display> = OnceCell::new();

struct FrameBufferResource {
    slot: u8,
    owner: u16,
    id: u32,
}

struct LayerResource {
    slot: u8,
    owner: u16,
}

fn lilla_format(fmt: ImageFormat) -> LillaFormat {
    match fmt {
        ImageFormat::Mono => LillaFormat::Mono,
        ImageFormat::TwoBpp => LillaFormat::TwoBpp,
        ImageFormat::EightBpp => LillaFormat::EightBpp,
    }
}

/// Build an `Image` from a validated `FrameBufferInfo`. Returns `Err` rather
/// than panicking on heap exhaustion so a busy compositor degrades gracefully.
fn try_make_image(info: FrameBufferInfo) -> Result<Image, FrameBufferError> {
    let len = info.data_len();
    let mut data: Vec<u8> = Vec::new();
    data.try_reserve_exact(len)
        .map_err(|_| FrameBufferError::OutOfMemory)?;
    data.resize(len, 0);
    let size = Size {
        w: info.width as i32,
        h: info.height as i32,
    };
    Image::from_vec(size, lilla_format(info.format), data).ok_or(FrameBufferError::InvalidDimensions)
}

fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    REGISTRY
        .get()
        .log_expect("registry not initialized")
        .with(f)
        .log_expect("reentrant registry access")
}

fn with_layers<R>(f: impl FnOnce(&mut Layers) -> R) -> R {
    LAYERS
        .get()
        .log_expect("layers not initialized")
        .with(f)
        .log_expect("reentrant layers access")
}

fn with_scratch<R>(f: impl FnOnce(&mut Scratch) -> R) -> R {
    SCRATCH
        .get()
        .log_expect("scratch not initialized")
        .with(f)
        .log_expect("reentrant scratch access")
}

impl FrameBuffer for FrameBufferResource {
    fn new(
        meta: ipc::Meta,
        info: FrameBufferInfo,
        seed_data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<Self, FrameBufferError> {
        if info.width == 0
            || info.height == 0
            || info.width > MAX_IMAGE_DIM
            || info.height > MAX_IMAGE_DIM
        {
            return Err(FrameBufferError::InvalidDimensions);
        }
        let expected = info.data_len();
        if seed_data.len() != expected {
            return Err(FrameBufferError::WrongSeedLength);
        }

        let mut image = try_make_image(info)?;
        let _ = seed_data.read_range(0, image.data.as_mut_slice());

        let owner = meta.sender.task_index();
        with_registry(move |r| {
            for (idx, slot) in r.slots.iter_mut().enumerate() {
                if slot.is_none() {
                    let id = r.next_id;
                    // Skip 0 on wrap so id == 0 never names a live framebuffer.
                    let next = id.wrapping_add(1);
                    r.next_id = if next == 0 { 1 } else { next };
                    *slot = Some(FrameBufferEntry { id, image });
                    return Ok(FrameBufferResource {
                        slot: idx as u8,
                        owner,
                        id,
                    });
                }
            }
            Err(FrameBufferError::OutOfSlots)
        })
    }

    fn write(
        &mut self,
        meta: ipc::Meta,
        seed_data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<(), FrameBufferError> {
        if meta.sender.task_index() != self.owner {
            return Err(FrameBufferError::NotOwner);
        }
        let slot = self.slot as usize;
        with_registry(|r| {
            let entry = r.slots[slot]
                .as_mut()
                .log_expect("frame buffer slot vanished while owned");
            if seed_data.len() != entry.image.data.len() {
                return Err(FrameBufferError::WrongSeedLength);
            }
            let _ = seed_data.read_range(0, entry.image.data.as_mut_slice());
            Ok(())
        })
    }

    fn id(&mut self, _meta: ipc::Meta) -> FrameBufferId {
        FrameBufferId(self.id)
    }
}

impl Drop for FrameBufferResource {
    fn drop(&mut self) {
        let slot = self.slot as usize;
        with_registry(|r| {
            r.slots[slot] = None;
        });
    }
}

impl Layer for LayerResource {
    fn new(
        meta: ipc::Meta,
        framebuffer: FrameBufferId,
        x: i16,
        y: i16,
        z: i16,
        blend: BlendMode,
    ) -> Result<Self, LayerError> {
        if !with_registry(|r| r.contains(framebuffer.0)) {
            return Err(LayerError::UnknownFrameBuffer);
        }
        let owner = meta.sender.task_index();
        with_layers(|layers| {
            for (idx, slot) in layers.slots.iter_mut().enumerate() {
                if slot.is_none() {
                    *slot = Some(LayerEntry {
                        framebuffer_id: framebuffer.0,
                        x,
                        y,
                        z,
                        _blend: blend,
                        visible: true,
                    });
                    return Ok(LayerResource {
                        slot: idx as u8,
                        owner,
                    });
                }
            }
            Err(LayerError::OutOfSlots)
        })
    }

    fn set_framebuffer(
        &mut self,
        meta: ipc::Meta,
        framebuffer: FrameBufferId,
    ) -> Result<(), LayerError> {
        if meta.sender.task_index() != self.owner {
            return Err(LayerError::NotOwner);
        }
        if !with_registry(|r| r.contains(framebuffer.0)) {
            return Err(LayerError::UnknownFrameBuffer);
        }
        let slot = self.slot as usize;
        with_layers(|layers| {
            if let Some(entry) = layers.slots[slot].as_mut() {
                entry.framebuffer_id = framebuffer.0;
            }
        });
        Ok(())
    }

    fn set_position(&mut self, meta: ipc::Meta, x: i16, y: i16) -> Result<(), LayerError> {
        if meta.sender.task_index() != self.owner {
            return Err(LayerError::NotOwner);
        }
        let slot = self.slot as usize;
        with_layers(|layers| {
            if let Some(entry) = layers.slots[slot].as_mut() {
                entry.x = x;
                entry.y = y;
            }
        });
        Ok(())
    }

    fn set_z(&mut self, meta: ipc::Meta, z: i16) -> Result<(), LayerError> {
        if meta.sender.task_index() != self.owner {
            return Err(LayerError::NotOwner);
        }
        let slot = self.slot as usize;
        with_layers(|layers| {
            if let Some(entry) = layers.slots[slot].as_mut() {
                entry.z = z;
            }
        });
        Ok(())
    }

    fn set_visible(&mut self, meta: ipc::Meta, visible: bool) -> Result<(), LayerError> {
        if meta.sender.task_index() != self.owner {
            return Err(LayerError::NotOwner);
        }
        let slot = self.slot as usize;
        with_layers(|layers| {
            if let Some(entry) = layers.slots[slot].as_mut() {
                entry.visible = visible;
            }
        });
        Ok(())
    }
}

impl Drop for LayerResource {
    fn drop(&mut self) {
        let slot = self.slot as usize;
        with_layers(|layers| {
            layers.slots[slot] = None;
        });
    }
}

/// Convert a 128×64 Mono `Image` to SSD1312 GDDRAM page format:
/// `height/8` pages of `width` bytes each, where each byte encodes 8 vertical
/// pixels with the LSB as the topmost pixel.
fn mono_to_ssd1312(src: &Image, out: &mut [u8; DISPLAY_BUF_SIZE]) {
    let width = DISPLAY_WIDTH as usize;
    out.fill(0);
    for page in 0..DISPLAY_PAGES {
        for col in 0..width {
            let mut byte = 0u8;
            for bit in 0..8u8 {
                let y = page as i32 * 8 + bit as i32;
                if graphics::read_pixel(src, Point { x: col as i32, y }) == Color::Black {
                    byte |= 1 << bit;
                }
            }
            out[page * width + col] = byte;
        }
    }
}

#[ipc::notification_handler(present)]
fn handle_present(_sender: u16, _code: u32) {
    let display = DISPLAY_HANDLE.get().log_expect("display not initialized");
    with_scratch(|s| {
        graphics::clear(&mut s.frame, None, Color::White);
        with_layers(|layers| {
            with_registry(|fbs| {
                // Build a stable index list of visible layers and sort by z.
                // Tie-break is allocation order via stable sort + ascending
                // input order. 32 entries → in-place sort is trivial.
                let mut entries: [(u8, i16); MAX_LAYERS] = [(0u8, 0i16); MAX_LAYERS];
                let mut count = 0usize;
                for (idx, slot) in layers.slots.iter().enumerate() {
                    if let Some(layer) = slot {
                        if layer.visible {
                            entries[count] = (idx as u8, layer.z);
                            count += 1;
                        }
                    }
                }
                entries[..count].sort_by_key(|&(_, z)| z);

                for &(idx, _) in &entries[..count] {
                    if let Some(layer) = layers.slots[idx as usize].as_ref() {
                        if let Some(image) = fbs.find_image(layer.framebuffer_id) {
                            graphics::draw_image(
                                &mut s.frame,
                                image,
                                Point {
                                    x: layer.x as i32,
                                    y: layer.y as i32,
                                },
                            );
                        }
                    }
                }
            });
        });
        mono_to_ssd1312(&s.frame, &mut s.out);
        display.draw(&s.out).log_expect("Failed to draw to display");
    });
}

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");

    // Bring the PSRAM region online as the global heap.
    let storage = FRAME_BUFFERS
        .get()
        .log_expect("frame buffer region already taken");
    // SAFETY: the region is freshly taken from `ipc::allocation!`, so it's
    // exclusively ours and lives forever.
    unsafe {
        HEAP.init(storage.as_mut_ptr() as *mut u8, FRAME_BUFFERS_BYTES);
    }

    let _ = REGISTRY.set(GlobalState::new(Registry {
        slots: core::array::from_fn(|_| None),
        next_id: 1,
    }));
    let _ = LAYERS.set(GlobalState::new(Layers {
        slots: core::array::from_fn(|_| None),
    }));

    // Pre-allocate the scratch frame and SSD1312 page buffer once, so
    // `handle_present` never allocates on the hot path.
    let frame = Image::new(
        Size {
            w: DISPLAY_WIDTH,
            h: DISPLAY_HEIGHT,
        },
        LillaFormat::Mono,
    );
    let _ = SCRATCH.set(GlobalState::new(Scratch {
        frame,
        out: [0u8; DISPLAY_BUF_SIZE],
    }));

    let display = DisplayConfiguration::builder(DISPLAY_WIDTH as u8, DISPLAY_HEIGHT as u8)
        .build()
        .open::<DisplayServer>()
        .log_expect("display IPC error")
        .ok()
        .log_expect("display already open");
    let _ = DISPLAY_HANDLE.set(display);
    rcard_log::info!("Display opened");

    ipc::server! {
        FrameBuffer: FrameBufferResource,
        Layer: LayerResource,
        @notifications(Reactor) => handle_present,
    }
}
