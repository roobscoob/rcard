#![no_std]
#![no_main]

use generated::notifications;
use generated::slots::SLOTS;
use rcard_log::{info, ResultExt};
use sysmodule_compositor_api::{BlendMode, FrameBufferInfo, ImageFormat};
use sysmodule_reactor_api::OverflowStrategy;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

sysmodule_compositor_api::bind_frame_buffer!(FrameBuffer = SLOTS.sysmodule_compositor);
sysmodule_compositor_api::bind_layer!(Layer = SLOTS.sysmodule_compositor);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

const PATTERN_W: u32 = 32;
const PATTERN_H: u32 = 32;
const PATTERN_PITCH: usize = (PATTERN_W as usize) / 8;
const PATTERN_BYTES: usize = PATTERN_PITCH * (PATTERN_H as usize);

/// Vertical stripes — every other column is black. In Mono format each row
/// is `0xAA` (`10101010`) repeated; MSB is the leftmost pixel.
const PATTERN_A: [u8; PATTERN_BYTES] = [0xAA; PATTERN_BYTES];

/// Horizontal stripes — even rows are entirely black, odd rows entirely white.
const PATTERN_B: [u8; PATTERN_BYTES] = {
    let mut b = [0u8; PATTERN_BYTES];
    let mut row = 0;
    while row < PATTERN_H as usize {
        if row % 2 == 0 {
            let mut col = 0;
            while col < PATTERN_PITCH {
                b[row * PATTERN_PITCH + col] = 0xFF;
                col += 1;
            }
        }
        row += 1;
    }
    b
};

/// Sleep for `ms` milliseconds by parking on a timer notification.
fn sleep_ms(ms: u64) {
    const TIMER_BIT: u32 = 1;
    let now = userlib::sys_get_timer().now;
    userlib::sys_set_timer(Some(now + ms), TIMER_BIT);
    let _ = userlib::sys_recv_notification(TIMER_BIT);
}

#[export_name = "main"]
fn main() -> ! {
    info!("fob: awake");

    let info_a = FrameBufferInfo::new(ImageFormat::Mono, PATTERN_W, PATTERN_H);
    let fb_a = FrameBuffer::new(info_a, &PATTERN_A)
        .log_expect("frame buffer A IPC")
        .log_expect("frame buffer A creation");

    let info_b = FrameBufferInfo::new(ImageFormat::Mono, PATTERN_W, PATTERN_H);
    let fb_b = FrameBuffer::new(info_b, &PATTERN_B)
        .log_expect("frame buffer B IPC")
        .log_expect("frame buffer B creation");

    let id_a = fb_a.id().log_expect("fb_a id");
    let id_b = fb_b.id().log_expect("fb_b id");

    // Layer A: vertical stripes, top-left of the overlap region.
    // Layer B: horizontal stripes, offset down-right with higher z so it
    // overwrites A in the overlap (the only thing z-order controls today,
    // since BlendMode::Replace is the only mode wired up).
    let _layer_a = Layer::new(id_a, 32, 16, 0, BlendMode::Replace)
        .log_expect("layer A IPC")
        .log_expect("layer A creation");
    let _layer_b = Layer::new(id_b, 48, 24, 1, BlendMode::Replace)
        .log_expect("layer B IPC")
        .log_expect("layer B creation");

    info!("fob: layers created, driving present at ~30 Hz");

    loop {
        let _ = Reactor::push(
            notifications::GROUP_ID_PRESENT,
            0,
            25,
            OverflowStrategy::DropOldest,
        );
        sleep_ms(33);
    }
}
