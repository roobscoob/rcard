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

// Direct LCPU control for the BLE bring-up — the bluetooth sysmodule is
// bypassed while we drive HCI from here.
sysmodule_lcpu_api::bind_lcpu!(Lcpu = SLOTS.sysmodule_lcpu);
sysmodule_mpr121_api::bind_mpr121!(Mpr121 = SLOTS.sysmodule_mpr121);
sysmodule_compositor_api::bind_frame_buffer!(FrameBuffer = SLOTS.sysmodule_compositor);
sysmodule_compositor_api::bind_layer!(Layer = SLOTS.sysmodule_compositor);
sysmodule_drv2603_api::bind_drv2603!(Haptic = SLOTS.sysmodule_drv2603);
sysmodule_power_api::bind_power!(Power = SLOTS.sysmodule_power);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

// ── Capacitive-touch bar display ──────────────────────────────────────

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
            match Power::charger_status() {
                Ok(status) => info!("charger: {} vbus={}", status.state, status.vbus_present),
                Err(e) => info!("charger status failed: {}", e),
            }

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
                info!("capA filt: {} {} {} {} {} {} {} {}",
                    top[4], top[5], top[6], top[7], top[8], top[9], top[10], top[11]);
                info!("capA base: {} {} {} {} {} {} {} {}",
                    top_bl[4], top_bl[5], top_bl[6], top_bl[7],
                    top_bl[8], top_bl[9], top_bl[10], top_bl[11]);
                info!("capB filt: {} {} {} {} {} {} {} {}",
                    bottom[4], bottom[5], bottom[6], bottom[7],
                    bottom[8], bottom[9], bottom[10], bottom[11]);
                info!("capB base: {} {} {} {} {} {} {} {}",
                    bottom_bl[4], bottom_bl[5], bottom_bl[6], bottom_bl[7],
                    bottom_bl[8], bottom_bl[9], bottom_bl[10], bottom_bl[11]);
            }

            draw_bars(&mut s.buf, &top, &top_bl, &bottom, &bottom_bl);
            draw_percentage(&mut s.buf, (rate * 100.0) as u32);
            let _ = s.fb.write(&s.buf);
        })
        .log_expect("reentrant touch state access");
}

// ── BLE HCI bring-up ──────────────────────────────────────────────────
//
// HCI command frames (H4 type byte + opcode + param_len + params).

/// HCI_Reset (OGF=0x03, OCF=0x0003 → opcode 0x0C03), no params.
const HCI_RESET: &[u8] = &[0x01, 0x03, 0x0C, 0x00];

/// HCI_LE_Set_Advertising_Parameters (opcode 0x2006).
/// min/max interval = 0x00A0 (= 100 ms in 0.625 ms units), ADV_IND,
/// public address, channels 37/38/39, allow any.
const HCI_LE_SET_ADV_PARAMS: &[u8] = &[
    0x01, 0x06, 0x20, 0x0F, // H4 cmd, opcode lo/hi, param_len = 15
    0xA0, 0x00, // adv_interval_min
    0xA0, 0x00, // adv_interval_max
    0x00, // adv_type = ADV_IND (connectable undirected)
    0x00, // own_address_type = public
    0x00, // peer_address_type = public
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // peer_address (unused with filter=0)
    0x07, // channel_map = ch 37/38/39
    0x00, // filter_policy = allow any
];

/// HCI_LE_Set_Advertising_Data (opcode 0x2008).
/// 10 bytes used: Flags AD + Complete Local Name "Charm". Remainder zero.
const HCI_LE_SET_ADV_DATA: &[u8] = &[
    0x01, 0x08, 0x20, 0x20, // H4 cmd, opcode lo/hi, param_len = 32
    0x0A, // adv_data_length = 10
    // AD #1: Flags (LE General Discoverable + BR/EDR not supported)
    0x02, 0x01, 0x06,
    // AD #2: Complete Local Name "Charm"
    0x06, 0x09, b'C', b'h', b'a', b'r', b'm',
    // 21 zero bytes of padding to reach the fixed 31-byte adv_data field
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// HCI_LE_Set_Advertising_Enable (opcode 0x200A), enable = 0x01.
const HCI_LE_SET_ADV_ENABLE: &[u8] = &[0x01, 0x0A, 0x20, 0x01, 0x01];

const OP_RESET: u16 = 0x0C03;
const OP_LE_SET_ADV_PARAMS: u16 = 0x2006;
const OP_LE_SET_ADV_DATA: u16 = 0x2008;
const OP_LE_SET_ADV_ENABLE: u16 = 0x200A;

/// Scan a recv'd HCI byte stream for a Command Complete event matching
/// `expected_opcode`. Returns the status byte if found. Handles multiple
/// concatenated events (we've seen LCPU coalesce two CCs in a single
/// recv when responses pile up).
fn find_cc(buf: &[u8], expected_opcode: u16) -> Option<u8> {
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] != 0x04 {
            return None; // not an HCI Event packet; bail
        }
        let evt_code = buf[i + 1];
        let param_len = buf[i + 2] as usize;
        let next = i + 3 + param_len;
        if next > buf.len() {
            return None;
        }
        if evt_code == 0x0E && param_len >= 4 {
            // Command Complete: num_hci_command_packets, opcode_lo, opcode_hi, status
            let cc_opcode = u16::from_le_bytes([buf[i + 4], buf[i + 5]]);
            if cc_opcode == expected_opcode {
                return Some(buf[i + 6]);
            }
        }
        i = next;
    }
    None
}

/// Drain whatever the reactor has queued so the queue doesn't fill up.
/// We don't care about the notification details — the bare bit waking
/// us is enough.
fn drain_reactor() {
    loop {
        match Reactor::pull() {
            Ok(Some(_)) => {}
            _ => break,
        }
    }
}

/// Send one HCI command and block until we see the matching Command
/// Complete. Logs along the way so we can see exactly where things
/// stall. Gives up silently on IPC errors — this is a debug hack.
fn send_and_await(lcpu: &mut Lcpu, cmd: &[u8], expected_opcode: u16) {
    info!("fob: sending opcode {}", expected_opcode);
    match lcpu.send_hci(cmd) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            info!("fob: send err: {}", e);
            return;
        }
        Err(e) => {
            info!("fob: send ipc err: {}", e);
            return;
        }
    }

    let mut buf = [0u8; 256];
    loop {
        let _ = userlib::sys_recv_notification(NOTIFICATION_BIT);
        drain_reactor();

        // Drain HCI bytes; LCPU may have written multiple events.
        loop {
            let n = match lcpu.recv_hci(&mut buf) {
                Ok(n) => n as usize,
                Err(_) => 0,
            };
            if n == 0 {
                break;
            }
            info!("fob: recv {} bytes", n);
            info!("fob: bytes: {}", buf[..n]);
            if let Some(status) = find_cc(&buf[..n], expected_opcode) {
                info!("fob: CC op={} status={}", expected_opcode, status);
                return;
            }
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    info!("fob: awake");

    // ── Power / charging ──
    match Power::charger_force_start() {
        Ok(Ok(())) => info!("fob: force charging enabled"),
        Ok(Err(e)) => info!("fob: force charge failed: {}", e),
        Err(e) => info!("fob: force charge IPC failed: {}", e),
    }

    // ── Capacitive touch + framebuffer + haptics ──
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

    // ── BLE bring-up: drive the LCPU directly and start advertising ──
    // The reactor queue (incl. `present`) is drained and discarded inside
    // send_and_await while we block on each Command Complete, so the touch
    // display simply doesn't refresh for the brief bring-up window.
    let bd_addr = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc];
    let mut lcpu = Lcpu::init(bd_addr)
        .log_expect("lcpu init ipc")
        .log_expect("lcpu init");
    info!("fob: lcpu ready");

    send_and_await(&mut lcpu, HCI_RESET, OP_RESET);
    send_and_await(&mut lcpu, HCI_LE_SET_ADV_PARAMS, OP_LE_SET_ADV_PARAMS);
    send_and_await(&mut lcpu, HCI_LE_SET_ADV_DATA, OP_LE_SET_ADV_DATA);
    send_and_await(&mut lcpu, HCI_LE_SET_ADV_ENABLE, OP_LE_SET_ADV_ENABLE);

    info!("fob: advertising!");

    // ── Unified service loop ──
    // A single notification bit wakes us for both the compositor's
    // `present` and the LCPU's `lcpu_data`. Each wake we drain any HCI
    // bytes the LCPU posted, then dispatch queued reactor notifications.
    // `handle_present` self-filters to the `present` group (its macro
    // guard skips `lcpu_data`), so the touch bars redraw on present and
    // HCI events are logged on lcpu_data.
    let mut hci_buf = [0u8; 256];
    loop {
        let _ = userlib::sys_recv_notification(NOTIFICATION_BIT);

        loop {
            let n = match lcpu.recv_hci(&mut hci_buf) {
                Ok(n) => n as usize,
                Err(_) => 0,
            };
            if n == 0 {
                break;
            }
            info!("fob: hci recv {} bytes: {}", n, hci_buf[..n]);
        }

        loop {
            match Reactor::pull() {
                Ok(Some(notif)) => handle_present(&notif),
                _ => break,
            }
        }
    }
}
