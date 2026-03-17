#![no_std]
#![no_main]

pub mod allocator;
pub mod frame_buffers;

use frame_buffers::FrameBuffers;
use hubris_task_slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
use rcard_log::{OptionExt, ResultExt};
use sysmodule_compositor_api::channel::ImageFormat;
use sysmodule_compositor_api::*;
use sysmodule_display_api::DisplayConfiguration;

sysmodule_display_api::bind_display!(Display = SLOTS.sysmodule_display);
sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log; cleanup Display, Reactor);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

mod generated {
    include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
}

static FRAME_BUFFERS: OnceCell<GlobalState<FrameBuffers>> = OnceCell::new();

fn with_frame_buffers<R>(f: impl FnOnce(&mut FrameBuffers) -> R) -> R {
    FRAME_BUFFERS
        .get()
        .log_expect("frame buffers not initialized")
        .with(f)
        .log_expect("reentrant frame buffer access")
}

const DISPLAY_WIDTH: usize = 128;
const DISPLAY_HEIGHT: usize = 64;
const DISPLAY_PAGES: usize = DISPLAY_HEIGHT / 8;
const DISPLAY_BUF_SIZE: usize = DISPLAY_WIDTH * DISPLAY_PAGES;

struct FrameBufferResource {
    allocation_id: u32,
    format: ImageFormat,
    owner: u16,
}

impl FrameBuffer for FrameBufferResource {
    fn new(
        meta: ipc::Meta,
        format: ImageFormat,
        seed_data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<Self, FrameBufferError> {
        let expected = format.storage_size_bytes();
        if seed_data.len() != expected {
            return Err(FrameBufferError::WrongSeedLength);
        }

        let mut buf = [0u8; frame_buffers::CHUNK_SIZE];
        with_frame_buffers(|fb| {
            let allocation_id = fb.next_id();
            let mut writer = fb
                .allocator_mut()
                .begin_write(allocation_id, expected)
                .map_err(|_| FrameBufferError::OutOfVram)?;

            let mut offset = 0;
            while offset < expected {
                let n = (expected - offset).min(buf.len());
                let _ = seed_data.read_range(offset, &mut buf[..n]);
                writer
                    .append(&buf[..n])
                    .map_err(|_| FrameBufferError::OutOfVram)?;
                offset += n;
            }

            Ok(FrameBufferResource {
                allocation_id,
                format,
                owner: meta.sender.task_index(),
            })
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
        let expected = self.format.storage_size_bytes();
        if seed_data.len() != expected {
            return Err(FrameBufferError::WrongSeedLength);
        }

        let mut buf = [0u8; frame_buffers::CHUNK_SIZE];
        let id = self.allocation_id;
        with_frame_buffers(|fb| {
            let mut writer = fb
                .allocator_mut()
                .begin_write(id, expected)
                .map_err(|_| FrameBufferError::OutOfVram)?;

            let mut offset = 0;
            while offset < expected {
                let n = (expected - offset).min(buf.len());
                let _ = seed_data.read_range(offset, &mut buf[..n]);
                writer
                    .append(&buf[..n])
                    .map_err(|_| FrameBufferError::OutOfVram)?;
                offset += n;
            }

            Ok(())
        })
    }
}

impl Drop for FrameBufferResource {
    fn drop(&mut self) {
        with_frame_buffers(|fb| {
            fb.free(self.allocation_id);
        });
    }
}

#[ipc::notification_handler(present)]
fn handle_present(_sender: u16, _code: u32) {
    let display = DISPLAY_HANDLE.get().log_expect("display not initialized");

    // Build a 1x1 checkerboard in SSD1312 GDDRAM format.
    // Each byte encodes 8 vertical pixels (LSB = topmost).
    // 0x55 = 0b01010101, 0xAA = 0b10101010.
    let mut buf = [0u8; DISPLAY_BUF_SIZE];
    for page in 0..DISPLAY_PAGES {
        for col in 0..DISPLAY_WIDTH {
            buf[page * DISPLAY_WIDTH + col] = if (page + col) % 2 == 0 { 0x55 } else { 0xAA };
        }
    }

    display.draw(&buf).log_expect("Failed to draw to display");
}

static DISPLAY_HANDLE: OnceCell<Display> = OnceCell::new();

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");
    let _ = FRAME_BUFFERS.set(GlobalState::new(FrameBuffers::take()));

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
        @notifications(Reactor) => handle_present,
    }
}
