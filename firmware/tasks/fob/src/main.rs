#![no_std]
#![no_main]

use generated::slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
use rcard_log::{info, OptionExt, ResultExt};
use sysmodule_compositor_api::{BlendMode, FrameBufferInfo, ImageFormat};
use sysmodule_reactor_api::NOTIFICATION_BIT;

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

/// Set a single pixel in a Mono framebuffer (MSB = leftmost column).
fn set_pixel(buf: &mut [u8; PATTERN_BYTES], x: i32, y: i32) {
    if x < 0 || y < 0 || x >= PATTERN_W as i32 || y >= PATTERN_H as i32 {
        return;
    }
    let byte = (y as usize) * PATTERN_PITCH + (x as usize) / 8;
    buf[byte] |= 0x80 >> ((x as usize) % 8);
}

/// Filled circle of `radius` pixels, centered in a 32×32 buffer.
fn draw_circle(buf: &mut [u8; PATTERN_BYTES], radius: u32) {
    buf.fill(0);
    let r = radius as i32;
    let r2 = r * r;
    for y in 0..PATTERN_H as i32 {
        for x in 0..PATTERN_W as i32 {
            let dx = x - 16;
            let dy = y - 16;
            if dx * dx + dy * dy <= r2 {
                set_pixel(buf, x, y);
            }
        }
    }
}

/// A `height`-row solid bar starting at the top of the buffer.
fn draw_bar(buf: &mut [u8; PATTERN_BYTES], height: u32) {
    buf.fill(0);
    let h = (height as usize).min(PATTERN_H as usize);
    for row in 0..h {
        for col in 0..PATTERN_PITCH {
            buf[row * PATTERN_PITCH + col] = 0xFF;
        }
    }
}

/// Triangle-wave bounce: cycles 0..=peak..=0 over `2 * peak` steps.
fn bounce(t: u32, peak: u32) -> u32 {
    if peak == 0 {
        return 0;
    }
    let p = t % (2 * peak);
    if p <= peak { p } else { 2 * peak - p }
}

/// Live animation state — mutated on every `present` notification from
/// the compositor. Held in a `OnceCell<GlobalState<…>>` so the
/// notification handler can borrow it without macro-generated args.
struct AnimState {
    fb_a: FrameBuffer,
    fb_b: FrameBuffer,
    layer_a: Layer,
    layer_b: Layer,
    buf_a: [u8; PATTERN_BYTES],
    buf_b: [u8; PATTERN_BYTES],
    frame: u32,
}

static STATE: OnceCell<GlobalState<AnimState>> = OnceCell::new();

#[ipc::notification_handler(present)]
fn handle_present(_sender: u16, _code: u32) {
    STATE
        .get()
        .log_expect("anim state not initialized")
        .with(|s| {
            // Display = 128×64, layers = 32×32. A slides on the top half,
            // B on the bottom half at half speed.
            let x_max = (128 - PATTERN_W) as u32; // 96
            let x_a = bounce(s.frame, x_max) as i16;
            let x_b = bounce(s.frame / 2, x_max) as i16;
            let _ = s.layer_a.set_position(x_a, 0);
            let _ = s.layer_b.set_position(x_b, 32);

            let radius = bounce(s.frame, 14);
            draw_circle(&mut s.buf_a, radius);
            let _ = s.fb_a.write(&s.buf_a);

            let height = bounce(s.frame, PATTERN_H);
            draw_bar(&mut s.buf_b, height);
            let _ = s.fb_b.write(&s.buf_b);

            s.frame = s.frame.wrapping_add(1);
        })
        .log_expect("reentrant anim state access");
}

#[export_name = "main"]
fn main() -> ! {
    info!("fob: awake");

    let mut buf_a = [0u8; PATTERN_BYTES];
    let mut buf_b = [0u8; PATTERN_BYTES];
    draw_circle(&mut buf_a, 8);
    draw_bar(&mut buf_b, 16);

    let info = FrameBufferInfo::new(ImageFormat::Mono, PATTERN_W, PATTERN_H);
    let fb_a = FrameBuffer::new(info, &buf_a)
        .log_expect("frame buffer A IPC")
        .log_expect("frame buffer A creation");
    let fb_b = FrameBuffer::new(info, &buf_b)
        .log_expect("frame buffer B IPC")
        .log_expect("frame buffer B creation");

    let id_a = fb_a.id().log_expect("fb_a id");
    let id_b = fb_b.id().log_expect("fb_b id");

    let layer_a = Layer::new(id_a, 0, 0, 0, BlendMode::Replace)
        .log_expect("layer A IPC")
        .log_expect("layer A creation");
    let layer_b = Layer::new(id_b, 0, 32, 1, BlendMode::Replace)
        .log_expect("layer B IPC")
        .log_expect("layer B creation");

    let _ = STATE.set(GlobalState::new(AnimState {
        fb_a,
        fb_b,
        layer_a,
        layer_b,
        buf_a,
        buf_b,
        frame: 0,
    }));

    info!("fob: layers created, listening for present");

    // Drain notifications via the reactor on every kernel-delivered wake.
    loop {
        let _ = userlib::sys_recv_notification(NOTIFICATION_BIT);
        loop {
            match Reactor::pull() {
                Ok(Some(notif)) => handle_present(&notif),
                _ => break,
            }
        }
    }
}
