#![no_std]
#![no_main]

use generated::slots::SLOTS;
use once_cell::{GlobalState, OnceCell};
use rcard_log::{info, OptionExt, ResultExt};
use sysmodule_compositor_api::{BlendMode, FrameBufferInfo, ImageFormat};
use sysmodule_mpr121_api::*;
use sysmodule_reactor_api::NOTIFICATION_BIT;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

sysmodule_bluetooth_api::bind_bluetooth!(Bt = SLOTS.sysmodule_bluetooth);
sysmodule_mpr121_api::bind_mpr121!(Mpr121 = SLOTS.sysmodule_mpr121);
sysmodule_compositor_api::bind_frame_buffer!(FrameBuffer = SLOTS.sysmodule_compositor);
sysmodule_compositor_api::bind_layer!(Layer = SLOTS.sysmodule_compositor);
sysmodule_drv2603_api::bind_drv2603!(Haptic = SLOTS.sysmodule_drv2603);
sysmodule_power_api::bind_power!(Power = SLOTS.sysmodule_power);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

const SCREEN_W: usize = 128;
const SCREEN_H: usize = 64;
const HALF_H: usize = SCREEN_H / 2;
const PITCH: usize = SCREEN_W / 8;
const FB_BYTES: usize = PITCH * SCREEN_H;

const NUM_CHANNELS: usize = 8;
const BAR_STRIDE: usize = SCREEN_W / NUM_CHANNELS;
const BAR_WIDTH: usize = BAR_STRIDE - 2;

// false = fixed scale (bars clip at MAX_DELTA, default)
// true  = auto scale (tallest bar always fills half-screen)
const AUTO_SCALE: bool = false;
const MAX_DELTA: i32 = 5;

fn set_pixel(buf: &mut [u8; FB_BYTES], x: usize, y: usize) {
    if x < SCREEN_W && y < SCREEN_H {
        let byte = y * PITCH + x / 8;
        buf[byte] |= 0x80 >> (x % 8);
    }
}

// 5x7 digit bitmaps (rows top-to-bottom, MSB-left within each u8)
const FONT_W: usize = 5;
const FONT_H: usize = 7;
#[rustfmt::skip]
const DIGITS: [[u8; 7]; 11] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110], // 0
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110], // 1
    [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111], // 2
    [0b01110, 0b10001, 0b00001, 0b00110, 0b00001, 0b10001, 0b01110], // 3
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010], // 4
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110], // 5
    [0b01110, 0b10000, 0b11110, 0b10001, 0b10001, 0b10001, 0b01110], // 6
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000], // 7
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110], // 8
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110], // 9
    [0b00000, 0b00000, 0b01110, 0b00000, 0b00000, 0b01110, 0b00000], // % (index 10)
];

fn draw_char(buf: &mut [u8; FB_BYTES], ch: usize, x0: usize, y0: usize) {
    if ch > 10 {
        return;
    }
    let glyph = &DIGITS[ch];
    let mut row = 0;
    while row < FONT_H {
        let bits = glyph[row];
        let mut col = 0;
        while col < FONT_W {
            if bits & (0b10000 >> col) != 0 {
                set_pixel(buf, x0 + col, y0 + row);
            }
            col += 1;
        }
        row += 1;
    }
}

fn draw_percentage(buf: &mut [u8; FB_BYTES], pct: u32) {
    let d2 = (pct / 100) as usize;
    let d1 = ((pct / 10) % 10) as usize;
    let d0 = (pct % 10) as usize;

    let show_hundreds = d2 > 0;
    let num_chars = if show_hundreds { 4 } else { 3 }; // digits + %
    let total_w = num_chars * (FONT_W + 1) - 1;
    let x_start = (SCREEN_W - total_w) / 2;
    let y_start = (SCREEN_H - FONT_H) / 2;

    let mut x = x_start;
    if show_hundreds {
        draw_char(buf, d2, x, y_start);
        x += FONT_W + 1;
    }
    draw_char(buf, d1, x, y_start);
    x += FONT_W + 1;
    draw_char(buf, d0, x, y_start);
    x += FONT_W + 1;
    draw_char(buf, 10, x, y_start); // %
}

fn draw_bars(
    buf: &mut [u8; FB_BYTES],
    top: &[i32; 12],
    top_bl: &[u8; 12],
    bottom: &[i32; 12],
    bottom_bl: &[u8; 12],
) {
    buf.fill(0);

    // Compute delta = (baseline << 2) - filtered for ELE4..11, clamped to >= 0.
    // All channels sit near zero when untouched regardless of trace length.
    let mut top_delta = [0i32; NUM_CHANNELS];
    let mut bottom_delta = [0i32; NUM_CHANNELS];
    let mut max_delta = 1i32;

    let mut i = 0;
    while i < NUM_CHANNELS {
        let td = ((top_bl[4 + i] as i32) << 2) - top[4 + i];
        let bd = ((bottom_bl[4 + i] as i32) << 2) - bottom[4 + i];
        let td = if td > 0 { td } else { 0 };
        let bd = if bd > 0 { bd } else { 0 };
        top_delta[i] = td;
        bottom_delta[i] = bd;
        if td > max_delta { max_delta = td; }
        if bd > max_delta { max_delta = bd; }
        i += 1;
    }

    let scale = if AUTO_SCALE { max_delta } else { MAX_DELTA };

    // Top half: Device B bars grow downward from row 0
    i = 0;
    while i < NUM_CHANNELS {
        let h = ((bottom_delta[i].min(scale) as u32 * HALF_H as u32) / scale as u32) as usize;
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

    // Bottom half: Device A bars grow upward from row 63, reversed
    i = 0;
    while i < NUM_CHANNELS {
        let h = ((top_delta[NUM_CHANNELS - 1 - i].min(scale) as u32 * HALF_H as u32) / scale as u32) as usize;
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

const SWEEP_PERIOD_MS: u64 = 10_000;
const SWEEP_HALF_MS: f32 = (SWEEP_PERIOD_MS / 2) as f32;
const CLICK_DURATION_MS: u64 = 2;
const INTERVAL_MIN_MS: f32 = 10.0;
const INTERVAL_MAX_MS: f32 = 500.0;

struct TouchState {
    touch_a: Mpr121,
    touch_b: Mpr121,
    fb: FrameBuffer,
    _layer: Layer,
    buf: [u8; FB_BYTES],
    last_click: u64,
    clicking: bool,
    click_start: u64,
    log_counter: u8,
}

static STATE: OnceCell<GlobalState<TouchState>> = OnceCell::new();

#[ipc::notification_handler(present)]
fn handle_present(_sender: u16, _code: u32) {
    STATE
        .get()
        .log_expect("touch state not initialized")
        .with(|s| {
            let now = userlib::sys_get_timer().now;

            // Triangle wave 0→1→0 over SWEEP_PERIOD_MS controls click rate
            let phase = (now % SWEEP_PERIOD_MS) as f32 / SWEEP_HALF_MS;
            let rate = if phase <= 1.0 { phase } else { 2.0 - phase };

            // rate 0→1 maps to interval MAX→MIN (slow to fast)
            let interval_ms = INTERVAL_MAX_MS - rate * (INTERVAL_MAX_MS - INTERVAL_MIN_MS);

            // End click after CLICK_DURATION_MS
            if s.clicking && now - s.click_start >= CLICK_DURATION_MS {
                let _ = Haptic::stop();
                s.clicking = false;
            }

            // Fire a new click when interval has elapsed
            if !s.clicking && now - s.last_click >= interval_ms as u64 {
                let _ = Haptic::drive(0.5);
                s.clicking = true;
                s.click_start = now;
                s.last_click = now;
            }

            let top = match s.touch_a.read() {
                Ok(Ok(v)) => v,
                _ => return,
            };
            let top_bl = match s.touch_a.read_baseline() {
                Ok(Ok(v)) => v,
                _ => return,
            };
            let bottom = match s.touch_b.read() {
                Ok(Ok(v)) => v,
                _ => return,
            };
            let bottom_bl = match s.touch_b.read_baseline() {
                Ok(Ok(v)) => v,
                _ => return,
            };

            s.log_counter = s.log_counter.wrapping_add(1);
            if s.log_counter % 8 == 0 {
                //info!("capA filt: {} {} {} {} {} {} {} {}",
                //    top[4], top[5], top[6], top[7], top[8], top[9], top[10], top[11]);
               // info!("capA base: {} {} {} {} {} {} {} {}",
                //    top_bl[4], top_bl[5], top_bl[6], top_bl[7],
               //     top_bl[8], top_bl[9], top_bl[10], top_bl[11]);
               // info!("capB filt: {} {} {} {} {} {} {} {}",
               //     bottom[4], bottom[5], bottom[6], bottom[7],
               //     bottom[8], bottom[9], bottom[10], bottom[11]);
               // info!("capB base: {} {} {} {} {} {} {} {}",
               //     bottom_bl[4], bottom_bl[5], bottom_bl[6], bottom_bl[7],
               //     bottom_bl[8], bottom_bl[9], bottom_bl[10], bottom_bl[11]);
            }

            draw_bars(&mut s.buf, &top, &top_bl, &bottom, &bottom_bl);
            draw_percentage(&mut s.buf, (rate * 100.0) as u32);
            let _ = s.fb.write(&s.buf);
        })
        .log_expect("reentrant touch state access");
}

#[export_name = "main"]
fn main() -> ! {
    info!("fob: awake");

    match Power::charger_force_start() {
        Ok(Ok(())) => info!("fob: force charging enabled"),
        Ok(Err(e)) => info!("fob: force charge failed: {}", e),
        Err(e) => info!("fob: force charge IPC failed: {}", e),
    }

    let config = Mpr121Config::auto_12ch_3v3();

    let touch_a = Mpr121::open(Device::A, config)
        .log_expect("mpr121 A IPC")
        .log_expect("mpr121 A open");

    let touch_b = Mpr121::open(Device::B, config)
        .log_expect("mpr121 B IPC")
        .log_expect("mpr121 B open");

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
        touch_a,
        touch_b,
        fb,
        _layer: layer,
        buf,
        last_click: 0,
        clicking: false,
        click_start: 0,
        log_counter: 0,
    }));

    info!("fob: touch bar display running, starting haptic sweep");

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
