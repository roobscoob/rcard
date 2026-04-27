#![no_std]
#![no_main]

use generated::slots::SLOTS;
use rcard_log::info;
use sysmodule_display_api::DisplayConfiguration;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

sysmodule_display_api::bind_display!(Display = SLOTS.sysmodule_display);

const WIDTH: usize = 128;
const HEIGHT: usize = 64;
const FB_SIZE: usize = WIDTH * (HEIGHT / 8);

const CHARM_WIDTH: usize = 44;
const CHARM_PAGES: usize = 2;
const CHARM_MASK: [u8; 88] = [
    // page 0
    0xE0, 0xF8, 0xFC, 0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xE0,
    0xF0, 0xF8, 0xF8, 0xF8, 0xF8, 0xF8, 0xF8, 0xF0, 0xFC, 0xFC, 0xFC, 0xFC, 0xFC, 0xF8, 0xF0, 0xF0,
    0xF0, 0xF0, 0xF0, 0xF0, 0xF0, 0xF0, 0xF0, 0xF0, 0xF0, 0x00, 0x00, 0x00, // page 1
    0x1F, 0x3F, 0x3F, 0x3F, 0x3F, 0x3F, 0x3F, 0x3F, 0x1F, 0x1F, 0x3F, 0x7F, 0x7F, 0x7F, 0x7F, 0x7F,
    0x3F, 0x1F, 0x0F, 0x1F, 0x1F, 0x1F, 0x1F, 0x1F, 0x1F, 0x3F, 0x3F, 0x3F, 0x3F, 0x3F, 0x1F, 0x1F,
    0x1F, 0x1F, 0x1F, 0x1F, 0x1F, 0x3F, 0x3F, 0x3F, 0x3F, 0x3F, 0x3F, 0x1F,
];
const CHARM_DATA: [u8; 88] = [
    // page 0
    0x00, 0x00, 0x80, 0x60, 0x10, 0x08, 0x04, 0x1C, 0x00, 0xA0, 0x78, 0x24, 0x9C, 0x00, 0x00, 0x00,
    0x00, 0x80, 0x40, 0x20, 0xA0, 0x00, 0x00, 0x00, 0x00, 0x40, 0x70, 0xA0, 0x40, 0x00, 0x00, 0xC0,
    0x00, 0x80, 0x40, 0xC0, 0x00, 0x80, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, // page 1
    0x00, 0x00, 0x07, 0x08, 0x08, 0x08, 0x04, 0x02, 0x00, 0x07, 0x02, 0x01, 0x0F, 0x10, 0x08, 0x06,
    0x00, 0x01, 0x02, 0x01, 0x03, 0x04, 0x02, 0x01, 0x00, 0x00, 0x07, 0x08, 0x04, 0x03, 0x00, 0x07,
    0x02, 0x01, 0x00, 0x07, 0x01, 0x00, 0x07, 0x08, 0x08, 0x04, 0x00, 0x00,
];

struct Xorshift32(u32);

impl Xorshift32 {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

fn set_pixel(fb: &mut [u8; FB_SIZE], x: usize, y: usize) {
    if x < WIDTH && y < HEIGHT {
        fb[x + (y / 8) * WIDTH] |= 1 << (y % 8);
    }
}

const NUM_STARS: usize = 500;
const CX: f32 = WIDTH as f32 / 2.0;
const CY: f32 = HEIGHT as f32 / 2.0;
const MAX_RADIUS: f32 = 80.0;
const SPEED_SCALE: f32 = 0.005;

#[derive(Clone, Copy)]
struct Star {
    x: f32,
    y: f32,
}

fn approx_sin(x: f32) -> f32 {
    // Bhaskara I approximation: sin(x) for x in [0, PI]
    // For full range, fold into [0, 2*PI] then handle sign.
    const TWO_PI: f32 = 6.2831853;
    const PI: f32 = 3.1415927;
    let mut x = x % TWO_PI;
    if x < 0.0 {
        x += TWO_PI;
    }
    let negate = x >= PI;
    if negate {
        x -= PI;
    }
    let val = 16.0 * x * (PI - x) / (5.0 * PI * PI - 4.0 * x * (PI - x));
    if negate {
        -val
    } else {
        val
    }
}

fn approx_cos(x: f32) -> f32 {
    approx_sin(x + 1.5707963)
}

fn spawn_star(rng: &mut Xorshift32) -> Star {
    let angle = (rng.next() as f32) * (6.2831853 / 4294967296.0);
    let r = (rng.next() % 1200) as f32 * 0.01; // 0–12 px radius
    Star {
        x: CX + r * approx_cos(angle),
        y: CY + r * approx_sin(angle),
    }
}

const FONT_3X5: [[u8; 3]; 10] = [
    [0x1F, 0x11, 0x1F], // 0
    [0x00, 0x1F, 0x00], // 1
    [0x1D, 0x15, 0x17], // 2
    [0x15, 0x15, 0x1F], // 3
    [0x07, 0x04, 0x1F], // 4
    [0x17, 0x15, 0x1D], // 5
    [0x1F, 0x15, 0x1D], // 6
    [0x01, 0x01, 0x1F], // 7
    [0x1F, 0x15, 0x1F], // 8
    [0x17, 0x15, 0x1F], // 9
];

fn draw_digit(fb: &mut [u8; FB_SIZE], x: usize, y: usize, digit: u8) {
    if digit > 9 { return; }
    let glyph = &FONT_3X5[digit as usize];
    for col in 0..3 {
        let bits = glyph[col];
        for row in 0..5 {
            if bits & (1 << row) != 0 {
                set_pixel(fb, x + col, y + row);
            }
        }
    }
}

fn draw_number(fb: &mut [u8; FB_SIZE], x: usize, y: usize, mut val: u32) {
    if val == 0 {
        draw_digit(fb, x, y, 0);
        return;
    }
    let mut digits = [0u8; 10];
    let mut len = 0;
    while val > 0 {
        digits[len] = (val % 10) as u8;
        val /= 10;
        len += 1;
    }
    for i in 0..len {
        draw_digit(fb, x + (len - 1 - i) * 4, y, digits[i]);
    }
}

fn blit_charm(fb: &mut [u8; FB_SIZE]) {
    let ox = (WIDTH - CHARM_WIDTH) / 2;
    let oy_page = (HEIGHT / 8 - CHARM_PAGES) / 2;
    let oy_bit = (HEIGHT - CHARM_PAGES * 8) / 2 % 8;

    for col in 0..CHARM_WIDTH {
        let fb_x = ox + col;
        if fb_x >= WIDTH {
            break;
        }
        for page in 0..CHARM_PAGES {
            let mask = CHARM_MASK[page * CHARM_WIDTH + col];
            let data = CHARM_DATA[page * CHARM_WIDTH + col];

            if oy_bit == 0 {
                let fb_page = oy_page + page;
                if fb_page < HEIGHT / 8 {
                    let idx = fb_page * WIDTH + fb_x;
                    fb[idx] = (fb[idx] & !mask) | (data & mask);
                }
            } else {
                let fb_page_lo = oy_page + page;
                let fb_page_hi = fb_page_lo + 1;
                if fb_page_lo < HEIGHT / 8 {
                    let shifted_mask = mask << oy_bit;
                    let shifted_data = data << oy_bit;
                    let idx = fb_page_lo * WIDTH + fb_x;
                    fb[idx] = (fb[idx] & !shifted_mask) | (shifted_data & shifted_mask);
                }
                if fb_page_hi < HEIGHT / 8 {
                    let shifted_mask = mask >> (8 - oy_bit);
                    let shifted_data = data >> (8 - oy_bit);
                    let idx = fb_page_hi * WIDTH + fb_x;
                    fb[idx] = (fb[idx] & !shifted_mask) | (shifted_data & shifted_mask);
                }
            }
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    let config = DisplayConfiguration::builder(WIDTH as u8, HEIGHT as u8)
        .flip_vertical()
        .no_charge_pump()
        .build();

    let display = config.open::<DisplayServer>().unwrap().unwrap();

    let mut rng = Xorshift32(0xDEAD_BEEF);

    let mut stars = [Star { x: CX, y: CY }; NUM_STARS];
    for star in stars.iter_mut() {
        *star = spawn_star(&mut rng);
        let dx = star.x - CX;
        let dy = star.y - CY;
        let spread = (rng.next() % 40) as f32 + 1.0;
        star.x = CX + dx * spread;
        star.y = CY + dy * spread;
    }

    let mut fb = [0u8; FB_SIZE];
    let mut contrast: u8 = 0;
    let mut frame_count: u32 = 0;
    let mut going_up = true;
    let mut fps: u32 = 0;
    let mut fps_frames: u32 = 0;
    let mut fps_last = userlib::sys_get_timer().now;

    loop {
        fb.fill(0);

        for star in stars.iter_mut() {
            let dx = star.x - CX;
            let dy = star.y - CY;
            let dist_sq = dx * dx + dy * dy;
            let speed = SPEED_SCALE + dist_sq * SPEED_SCALE * 0.001;
            star.x += dx * speed;
            star.y += dy * speed;

            if star.x < 0.0
                || star.x >= WIDTH as f32
                || star.y < 0.0
                || star.y >= HEIGHT as f32
                || dist_sq > MAX_RADIUS * MAX_RADIUS
            {
                *star = spawn_star(&mut rng);
                continue;
            }

            set_pixel(&mut fb, star.x as usize, star.y as usize);
        }

        blit_charm(&mut fb);
        draw_number(&mut fb, 1, 1, contrast as u32);

        let fps_digits = if fps == 0 { 1usize } else {
            let mut n = fps;
            let mut d = 0usize;
            while n > 0 { d += 1; n /= 10; }
            d
        };
        draw_number(&mut fb, WIDTH - fps_digits * 4, 1, fps);

        let _ = display.draw(&fb);

        frame_count += 1;
        fps_frames += 1;
        let now = userlib::sys_get_timer().now;
        if now - fps_last >= 1000 {
            fps = fps_frames;
            fps_frames = 0;
            fps_last = now;
        }

        if frame_count % 4 == 0 {
            if going_up {
                if contrast == 255 {
                    going_up = false;
                } else {
                    contrast += 1;
                }
            } else {
                if contrast == 0 {
                    going_up = true;
                } else {
                    contrast -= 1;
                }
            }
            let _ = display.set_contrast(contrast);
        }
    }
}
