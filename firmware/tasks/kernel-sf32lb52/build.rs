#![allow(clippy::unwrap_used)]

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Provide device.x with DefaultHandler entries for all SF32LB52 IRQs.
    // The PAC's __INTERRUPTS is [Vector; 0] (svd2rust didn't extract IRQs),
    // so we must provide the vector table ourselves.
    // SF32LB52 has IRQs up to 98, so we need 99 entries.
    let mut device_x = String::new();
    for i in 0..99 {
        device_x.push_str(&format!("PROVIDE(Interrupt{} = DefaultHandler);\n", i));
    }
    fs::write(out.join("device.x"), &device_x).unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    // Generate pin configuration code from .pins.json
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let work_dir = manifest_dir
        .ancestors()
        .find(|p| p.join(".work").exists())
        .map(|p| p.join(".work"));

    let mut pin_code = String::new();
    writeln!(pin_code, "/// Apply all board pin assignments.").unwrap();
    writeln!(pin_code, "///").unwrap();
    writeln!(pin_code, "/// Generated from .pins.json by build.rs.").unwrap();
    writeln!(pin_code, "///").unwrap();
    writeln!(pin_code, "/// # Safety").unwrap();
    writeln!(pin_code, "///").unwrap();
    writeln!(pin_code, "/// Writes to HPSYS_PINMUX and HPSYS_CFG registers.").unwrap();
    writeln!(pin_code, "/// Must be called before any peripheral that depends on pin routing.").unwrap();
    writeln!(pin_code, "pub unsafe fn apply_pin_config() {{").unwrap();

    if let Some(work_dir) = work_dir {
        let json_path = work_dir.join("app.pins.json");
        println!("cargo:rerun-if-changed={}", json_path.display());

        if json_path.exists() {
            let content = fs::read_to_string(&json_path).unwrap();
            let root: serde_json::Value = serde_json::from_str(&content).unwrap();

            if let Some(assignments) = root.get("assignments").and_then(|v| v.as_array()) {
                for a in assignments {
                    let pin = a["pin"].as_str().unwrap();
                    let kind = a["kind"].as_str().unwrap();
                    let instance = a.get("instance").and_then(|v| v.as_str());
                    let signal = a["signal"].as_str().unwrap();

                    let pin_num = pin.strip_prefix("PA").unwrap().parse::<u32>().unwrap();

                    let writes = gen_pin_writes(pin_num, kind, instance, signal);
                    for line in &writes {
                        writeln!(pin_code, "    {line}").unwrap();
                    }
                }
            }
        }
    }

    writeln!(pin_code, "}}").unwrap();
    fs::write(out.join("pin_config.rs"), &pin_code).unwrap();
}

/// HPSYS_PINMUX base
const PINMUX_BASE: u32 = 0x5000_3000;
/// HPSYS_CFG base (PINR registers)
const CFG_BASE: u32 = 0x5000_B000;

/// PAD register address for a given pin number
const fn pad_addr(pin: u32) -> u32 {
    PINMUX_BASE + 0x34 + pin * 4
}

/// Signal direction — determines whether IE (input enable) must be set
fn is_input_signal(kind: &str, signal: &str) -> bool {
    match (kind, signal) {
        // USART
        ("usart", "rx") | ("usart", "cts") => true,
        ("usart", _) => false,
        // I2C is bidirectional — always needs IE
        ("i2c", _) => true,
        // SPI
        ("spi", "di") => true,
        ("spi", _) => false,
        // Timer inputs (ETR = external trigger)
        ("gptim", "etr") => true,
        ("gptim", _) => false,
        ("lptim", "in") | ("lptim", "etr") => true,
        ("atim", "etr") | ("atim", "bk") | ("atim", "bk2") => true,
        // GPADC is analog input
        ("gpadc", _) => true,
        // Default: output
        _ => false,
    }
}

/// For GPIO, direction is encoded in the instance (in/out/bidir)
fn is_gpio_input(instance: Option<&str>) -> bool {
    matches!(instance, Some("in") | Some("bidir"))
}

/// FSEL value for PINR-routed groups
fn pinr_fsel(kind: &str) -> Option<u32> {
    match kind {
        "usart" | "i2c" => Some(4),            // PA_I2C_UART
        "gptim" | "lptim" | "atim" => Some(5), // PA_TIM
        _ => None,
    }
}

/// FSEL value for dedicated (non-PINR) peripherals per pin
fn dedicated_fsel(kind: &str, instance: Option<&str>, pin: u32) -> Option<u32> {
    match (kind, instance) {
        // LCDC SPI — FSEL 1, pins PA00–PA08
        ("lcdc", Some("spi")) if pin <= 8 => Some(1),
        // LCDC 8080 — FSEL 7
        ("lcdc", Some("8080")) => Some(7),
        // LCDC JDI — FSEL 6
        ("lcdc", Some("jdi")) => Some(6),
        // SPI1 — FSEL 2
        ("spi", Some("1")) if matches!(pin, 24 | 25 | 28 | 29) => Some(2),
        // SPI2 — FSEL 2
        ("spi", Some("2")) if matches!(pin, 37..=40) => Some(2),
        // I2S1 — FSEL 3
        ("i2s", Some("1")) => Some(3),
        // PDM1 — FSEL 3
        ("pdm", Some("1")) => Some(3),
        // MPI2 — FSEL 1
        ("mpi", Some("2")) if matches!(pin, 12..=17) => Some(1),
        // SD1 — FSEL 2
        ("sd", Some("1")) if matches!(pin, 12..=17) => Some(2),
        // SWD — FSEL 2
        ("swd", _) if matches!(pin, 18 | 19) => Some(2),
        // USB11 — FSEL 2
        ("usb11", _) if matches!(pin, 35 | 36) => Some(2),
        // EFUSE — FSEL 2
        ("efuse", _) if pin == 30 => Some(2),
        // GPADC — FSEL 7
        ("gpadc", _) if matches!(pin, 28..=34) => Some(7),
        // XTAL32K — FSEL 8 (PA22/23) or FSEL 7 (PA25 ext)
        ("xtal32k", _) if matches!(pin, 22 | 23) => Some(8),
        ("xtal32k", _) if pin == 25 => Some(7),
        // WKUP — FSEL 8
        ("wkup", _) => Some(8),
        // GPIO — FSEL 0
        ("gpio", _) => Some(0),
        _ => None,
    }
}

/// PINR register offset (relative to CFG_BASE) and bit position for a signal
fn pinr_info(kind: &str, instance: Option<&str>, signal: &str) -> Option<(u32, u32)> {
    let inst: u32 = instance?.parse().ok()?;

    match kind {
        "usart" => {
            let offset = 0x58 + (inst - 1) * 4; // USART1=0x58, USART2=0x5C, USART3=0x60
            let shift = match signal {
                "tx" => 0,
                "rx" => 8,
                "cts" => 16,
                "rts" => 24,
                _ => return None,
            };
            Some((offset, shift))
        }
        "i2c" => {
            let offset = 0x48 + (inst - 1) * 4; // I2C1=0x48..I2C4=0x54
            let shift = match signal {
                "scl" => 0,
                "sda" => 8,
                _ => return None,
            };
            Some((offset, shift))
        }
        "gptim" => {
            if signal == "etr" {
                // ETR uses a separate register: ETR_PINR at 0x6C
                let shift = (inst - 1) * 8; // ETR1_PIN=[5:0], ETR2_PIN=[13:8]
                return Some((0x6C, shift));
            }
            let offset = 0x64 + (inst - 1) * 4; // GPTIM1=0x64, GPTIM2=0x68
            let shift = match signal {
                "ch1" => 0,
                "ch2" => 8,
                "ch4" => 16,
                "ch3" => 24,
                _ => return None,
            };
            Some((offset, shift))
        }
        "lptim" => {
            let offset = 0x70 + (inst - 1) * 4; // LPTIM1=0x70, LPTIM2=0x74
            let shift = match signal {
                "in" => 0,
                "out" => 8,
                "etr" => 16,
                _ => return None,
            };
            Some((offset, shift))
        }
        "atim" => {
            // ATIM1 only (inst must be 1)
            if inst != 1 {
                return None;
            }
            match signal {
                // PINR1 at 0x78
                "ch1" => Some((0x78, 0)),
                "ch2" => Some((0x78, 8)),
                "ch4" => Some((0x78, 16)),
                "ch3" => Some((0x78, 24)),
                // PINR2 at 0x7C
                "ch1n" => Some((0x7C, 0)),
                "ch2n" => Some((0x7C, 8)),
                "ch3n" => Some((0x7C, 16)),
                // PINR3 at 0x80
                "bk" => Some((0x80, 0)),
                "bk2" => Some((0x80, 8)),
                "etr" => Some((0x80, 16)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Generate the volatile register write statements for a single pin assignment.
fn gen_pin_writes(pin: u32, kind: &str, instance: Option<&str>, signal: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let pad = pad_addr(pin);
    let needs_ie = if kind == "gpio" {
        is_gpio_input(instance)
    } else {
        is_input_signal(kind, signal)
    };

    let desc = if let Some(inst) = instance {
        format!("{kind} {inst} {signal}")
    } else {
        format!("{kind} {signal}")
    };
    lines.push(format!("// PA{pin:02} -> {desc}"));

    // Determine FSEL value
    let fsel = if let Some(f) = pinr_fsel(kind) {
        f
    } else if let Some(f) = dedicated_fsel(kind, instance, pin) {
        f
    } else {
        lines.push(format!(
            "// WARNING: unknown FSEL for {kind} on PA{pin:02}, skipping"
        ));
        return lines;
    };

    // Step 1: Write PAD register (FSEL + optional IE)
    if needs_ie {
        // Clear FSEL[3:0] and IE[6], then set both
        let val = (1u32 << 6) | fsel; // IE=1, FSEL=fsel
        let mask = 0x4F; // bits [6] and [3:0]
        lines.push(format!("let pad = 0x{pad:08X} as *mut u32;"));
        lines.push(format!(
            "pad.write_volatile((pad.read_volatile() & !0x{mask:X}) | 0x{val:X});"
        ));
    } else if fsel == 0 {
        lines.push(format!("let pad = 0x{pad:08X} as *mut u32;"));
        lines.push("pad.write_volatile(pad.read_volatile() & !0xF);".to_string());
    } else {
        lines.push(format!("let pad = 0x{pad:08X} as *mut u32;"));
        lines.push(format!(
            "pad.write_volatile((pad.read_volatile() & !0xF) | {fsel});"
        ));
    }

    // Step 2: Write PINR register if this is a routed (FSEL 4/5) peripheral
    if pinr_fsel(kind).is_some() {
        #[allow(clippy::panic)]
        let (pinr_offset, shift) = pinr_info(kind, instance, signal).unwrap_or_else(|| {
            panic!(
                "PA{pin:02}: {kind} requires an instance and valid signal, \
                     got instance={instance:?} signal={signal:?}"
            )
        });
        let pinr_addr = CFG_BASE + pinr_offset;
        let mask = 0x3Fu32 << shift;
        let val = (pin & 0x3F) << shift;
        lines.push(format!("let pinr = 0x{pinr_addr:08X} as *mut u32;"));
        lines.push(format!(
            "pinr.write_volatile((pinr.read_volatile() & !0x{mask:X}) | 0x{val:X});"
        ));
    }

    lines.push(String::new()); // blank line between assignments
    lines
}
