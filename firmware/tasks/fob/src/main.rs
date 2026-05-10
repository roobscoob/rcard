#![no_std]
#![no_main]

use generated::slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
use rcard_log::{info, OptionExt, ResultExt};
use sysmodule_cap1208_api::*;
use sysmodule_compositor_api::{BlendMode, FrameBufferInfo, ImageFormat};
use sysmodule_reactor_api::NOTIFICATION_BIT;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

sysmodule_cap1208_api::bind_cap1208!(Cap1208 = SLOTS.sysmodule_cap1208);
sysmodule_compositor_api::bind_frame_buffer!(FrameBuffer = SLOTS.sysmodule_compositor);
sysmodule_compositor_api::bind_layer!(Layer = SLOTS.sysmodule_compositor);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

const SCREEN_W: usize = 128;
const SCREEN_H: usize = 64;
const HALF_H: usize = SCREEN_H / 2;
const PITCH: usize = SCREEN_W / 8;
const FB_BYTES: usize = PITCH * SCREEN_H;

const NUM_CHANNELS: usize = 8;
const BAR_STRIDE: usize = SCREEN_W / NUM_CHANNELS; // 16 px per bar
const BAR_WIDTH: usize = BAR_STRIDE - 2; // 14 px bar, 2 px gap

fn set_pixel(buf: &mut [u8; FB_BYTES], x: usize, y: usize) {
    let byte = y * PITCH + x / 8;
    buf[byte] |= 0x80 >> (x % 8);
}

fn draw_bars(buf: &mut [u8; FB_BYTES], top: &[i32; 8], bottom: &[i32; 8]) {
    buf.fill(0);

    // Auto-scale: find min/max across all 16 values
    let mut min = top[0];
    let mut max = top[0];
    let mut i = 0;
    while i < 8 {
        if top[i] < min {
            min = top[i];
        }
        if top[i] > max {
            max = top[i];
        }
        if bottom[i] < min {
            min = bottom[i];
        }
        if bottom[i] > max {
            max = bottom[i];
        }
        i += 1;
    }

    let range = max - min;

    // Top half: Device A bars grow downward from row 0
    i = 0;
    while i < NUM_CHANNELS {
        let h = if range > 0 {
            (((top[7 - i] - min) as u32 * HALF_H as u32) / range as u32) as usize
        } else {
            1
        };
        let x0 = i * BAR_STRIDE + 1;
        let mut row = 0;
        while row < h {
            let mut col = 0;
            while col < BAR_WIDTH {
                set_pixel(buf, x0 + col, row);
                col += 1;
            }
            row += 1;
        }
        i += 1;
    }

    // Bottom half: Device B bars grow upward from row 63
    i = 0;
    while i < NUM_CHANNELS {
        let h = if range > 0 {
            (((bottom[i] - min) as u32 * HALF_H as u32) / range as u32) as usize
        } else {
            1
        };
        let x0 = i * BAR_STRIDE + 1;
        let mut row = 0;
        while row < h {
            let y = SCREEN_H - 1 - row;
            let mut col = 0;
            while col < BAR_WIDTH {
                set_pixel(buf, x0 + col, y);
                col += 1;
            }
            row += 1;
        }
        i += 1;
    }
}

struct TouchState {
    cap_a: Cap1208,
    cap_b: Cap1208,
    fb: FrameBuffer,
    _layer: Layer,
    buf: [u8; FB_BYTES],
}

static STATE: OnceCell<GlobalState<TouchState>> = OnceCell::new();

#[ipc::notification_handler(present)]
fn handle_present(_sender: u16, _code: u32) {
    STATE
        .get()
        .log_expect("touch state not initialized")
        .with(|s| {
            let top = match s.cap_a.read() {
                Ok(Ok(v)) => v,
                _ => return,
            };
            let bottom = match s.cap_b.read() {
                Ok(Ok(v)) => v,
                _ => return,
            };

            draw_bars(&mut s.buf, &top, &bottom);
            let _ = s.fb.write(&s.buf);
        })
        .log_expect("reentrant touch state access");
}

#[export_name = "main"]
fn main() -> ! {
    info!("fob: awake");

    let config = Cap1208Config {
        enabled_channels: 0xFF,
        signal: SignalConfig::new(AnalogGain::X1, DigitalShift::X1),
        sampling: SamplingConfig::fastest(Averaging::Avg4, Duration::Us640),
        recalibration: RecalibrationConfig::every(RecalRate::Samples32)
            .with_touch_duration(TouchRecalDuration::Disabled)
            .with_below_baseline(BelowBaseline::Count32),
    };

    let cap_a = Cap1208::open(Device::A, config)
        .log_expect("cap1208 A IPC")
        .log_expect("cap1208 A open");

    let cap_b = Cap1208::open(Device::B, config)
        .log_expect("cap1208 B IPC")
        .log_expect("cap1208 B open");

    info!("fob: touch sensors open");

    let buf = [0u8; FB_BYTES];
    let info = FrameBufferInfo::new(ImageFormat::Mono, SCREEN_W as u32, SCREEN_H as u32);
    let fb = FrameBuffer::new(info, &buf)
        .log_expect("fb IPC")
        .log_expect("fb creation");
    let fb_id = fb.id().log_expect("fb id");
    let layer = Layer::new(fb_id, 0, 0, 0, BlendMode::Replace)
        .log_expect("layer IPC")
        .log_expect("layer creation");

    let _ = STATE.set(GlobalState::new(TouchState {
        cap_a,
        cap_b,
        fb,
        _layer: layer,
        buf,
    }));

    info!("fob: touch bar display running");

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
