#![no_std]
#![no_main]

use core::sync::atomic::{AtomicBool, Ordering};

use generated::slots::SLOTS;
use sysmodule_display_api::*;

static DISPLAY_IN_USE: AtomicBool = AtomicBool::new(false);

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);

fn lcdc() -> sifli_pac::lcdc::Lcdc {
    sifli_pac::LCDC1
}

fn gpio() -> sifli_pac::hpsys_gpio::HpsysGpio {
    sifli_pac::HPSYS_GPIO
}

/// PA02 — display_en
const DISPLAY_EN_BIT: u32 = 1 << 2;

fn set_display_en(on: bool) {
    let g = gpio();
    if on {
        g.doesr0().write(|w| w.0 = DISPLAY_EN_BIT);
        g.dosr0().write(|w| w.0 = DISPLAY_EN_BIT);
    } else {
        g.docr0().write(|w| w.0 = DISPLAY_EN_BIT);
    }
}

fn busy_wait(cycles: u32) {
    for _ in 0..cycles {
        core::hint::spin_loop();
    }
}

/// Send a single byte over the LCDC's SPI interface.
///
/// `is_data` selects the D/C# pin state: true = data (GDDRAM write),
/// false = command.
fn spi_send(is_data: bool, byte: u8) {
    let l = lcdc();
    // bit 3: LCD_BUSY — poll until the previous transfer completes
    while l.lcd_single().read().0 & (1 << 3) != 0 {}
    // bit 0: TYPE — 0 = command (D/C# low), 1 = data (D/C# high)
    l.lcd_single().write(|w| w.0 = if is_data { 1 } else { 0 });
    l.lcd_wr().write(|w| w.0 = byte as u32);
    // bit 1: WR_TRIG — triggers the SPI write; TYPE must remain set
    l.lcd_single()
        .write(|w| w.0 = if is_data { 1 | (1 << 1) } else { 1 << 1 });
}

fn ssd1312_cmd(byte: u8) {
    spi_send(false, byte);
}

fn ssd1312_data(byte: u8) {
    spi_send(true, byte);
}

fn ssd1312_cmd_arg(cmd: u8, arg: u8) {
    ssd1312_cmd(cmd);
    ssd1312_cmd(arg);
}

/// Configure the LCDC for 4-wire SPI mode and reset the display.
fn lcdc_init_spi() {
    let l = lcdc();
    // LCD_CONF: bits [4:2] LCD_INTF_SEL = 1 (SPI interface)
    l.lcd_conf().write(|w| w.0 = 1 << 2);
    // SPI_IF_CONF: bit 27 SPI_CS_AUTO_DIS = 1, bits [13:6] CLK_DIV = 4,
    //              bits [19:17] LINE = 0 (4-wire SPI)
    l.spi_if_conf().write(|w| w.0 = (1 << 27) | (4 << 6));
    // LCD_IF_CONF: LCD_RSTB = 0 (assert reset),
    //              PWH[17:12] = 1, PWL[11:6] = 1, TAH[5:3] = 1, TAS[2:0] = 1
    l.lcd_if_conf()
        .write(|w| w.0 = (1 << 12) | (1 << 6) | (1 << 3) | 1);
    // LCD_IF_CONF: bit 23 LCD_RSTB = 1 (release reset), same timing
    l.lcd_if_conf()
        .write(|w| w.0 = (1 << 23) | (1 << 12) | (1 << 6) | (1 << 3) | 1);
    // Wait for SSD1312 to complete internal reset (~3us minimum)
    busy_wait(1_000);
}

fn ssd1312_init(config: &DisplayConfiguration) {
    let width = config.width;
    let height = config.height;

    ssd1312_cmd(0xAE); // Display off

    ssd1312_cmd_arg(0xD5, 0x80); // Clock divide / oscillator
    ssd1312_cmd_arg(0xA8, height - 1); // MUX ratio
    ssd1312_cmd_arg(0xD3, 0x00); // Display offset = 0
    ssd1312_cmd(0x40); // Start line = 0

    // Charge pump
    ssd1312_cmd_arg(0x8D, if config.charge_pump { 0x12 } else { 0x10 });

    ssd1312_cmd_arg(0x20, 0x02); // Page addressing mode

    // Segment remap
    ssd1312_cmd(if config.segment_remap { 0xA1 } else { 0xA0 });

    // COM scan direction
    ssd1312_cmd(if config.com_reversed { 0xC8 } else { 0xC0 });

    ssd1312_cmd_arg(0xDA, config.com_pin_config); // COM pins config
    ssd1312_cmd_arg(0x81, config.contrast); // Contrast
    ssd1312_cmd_arg(0xD9, 0x22); // Pre-charge period
    ssd1312_cmd_arg(0xDB, 0x20); // VCOMH deselect

    // Normal / inverted
    ssd1312_cmd(if config.invert { 0xA7 } else { 0xA6 });

    ssd1312_cmd(0xA4); // Resume from entire display ON

    // Clear GDDRAM to all zeros (black) — contents are undefined after reset
    let pages = height / 8;
    for page in 0..pages {
        ssd1312_cmd(0xB0 | page); // Set page address
        ssd1312_cmd(0x00); // Lower column = 0
        ssd1312_cmd(0x10); // Upper column = 0
        for _ in 0..width {
            ssd1312_data(0x00);
        }
    }

    ssd1312_cmd(0xAF); // Display on
}

struct DisplayResource {
    config: DisplayConfiguration,
}

impl Display for DisplayResource {
    fn open(_meta: ipc::Meta, config: DisplayConfiguration) -> Result<Self, DisplayOpenError> {
        if DISPLAY_IN_USE.swap(true, Ordering::Acquire) {
            return Err(DisplayOpenError::AlreadyOpen);
        }

        set_display_en(true);
        lcdc_init_spi();
        ssd1312_init(&config);

        Ok(DisplayResource { config })
    }

    fn draw(
        &mut self,
        _meta: ipc::Meta,
        framebuffer: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) {
        let width = self.config.width as usize;
        let height = self.config.height;
        let pages = height / 8;
        let mut row_buf = [0u8; 255];
        for page in 0..pages {
            ssd1312_cmd(0xB0 | page);
            ssd1312_cmd(0x00);
            ssd1312_cmd(0x10);
            let row_start = (page as usize) * width;
            let _ = framebuffer.read_range(row_start, &mut row_buf[..width]);
            let row_buf = &row_buf[..width];
            for col in row_buf {
                ssd1312_data(*col);
            }
        }
    }
}

impl Drop for DisplayResource {
    fn drop(&mut self) {
        ssd1312_cmd(0xAE); // Display off
        set_display_en(false);
        DISPLAY_IN_USE.store(false, Ordering::Release);
    }
}

#[export_name = "main"]
fn main() -> ! {
    rcard_log::info!("Awake");
    ipc::server! {
        Display: DisplayResource,
    }
}
