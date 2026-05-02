#![no_std]
#![no_main]

use generated::slots::SLOTS;
use rcard_log::{info, ResultExt};
use sysmodule_compositor_api::{BlendMode, FrameBufferInfo, ImageFormat};

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

sysmodule_compositor_api::bind_frame_buffer!(FrameBuffer = SLOTS.sysmodule_compositor);
sysmodule_compositor_api::bind_layer!(Layer = SLOTS.sysmodule_compositor);

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

    info!("fob: layers created, animating");

    // Display = 128×64, layers = 32×32. Each layer bounces along one axis:
    //   A on the top half, sliding horizontally
    //   B on the bottom half, sliding horizontally at half speed
    // Each layer's contents also animate — A's circle pulses, B's bar fills/drains.
    let x_max_a = (128 - PATTERN_W) as u32; // 96
    let x_max_b = (128 - PATTERN_W) as u32; // 96
    let mut frame: u32 = 0;
    loop {
        // Position animation.
        let x_a = bounce(frame, x_max_a) as i16;
        let x_b = bounce(frame / 2, x_max_b) as i16;
        let _ = layer_a.set_position(x_a, 0);
        let _ = layer_b.set_position(x_b, 32);

        // Content animation.
        let radius = bounce(frame, 14); // 0..=14..=0
        draw_circle(&mut buf_a, radius);
        let _ = fb_a.write(&buf_a);

        let height = bounce(frame, PATTERN_H); // 0..=32..=0
        draw_bar(&mut buf_b, height);
        let _ = fb_b.write(&buf_b);

        frame = frame.wrapping_add(1);
    }
}
