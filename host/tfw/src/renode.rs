use std::fmt::Write;

use crate::config::AppConfig;

/// Generate a Renode platform description (.repl) from the config.
pub fn generate_repl(config: &AppConfig) -> String {
    let mut out = String::new();

    // CPU + NVIC
    writeln!(out, "cpu: CPU.CortexM @ sysbus").unwrap();
    writeln!(out, "    cpuType: \"cortex-m33\"").unwrap();
    writeln!(out, "    nvic: nvic").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "nvic: IRQControllers.NVIC @ sysbus 0xE000E000").unwrap();
    writeln!(out, "    -> cpu@0").unwrap();
    writeln!(out).unwrap();

    // Memory regions
    writeln!(out, "// Memory").unwrap();
    writeln!(out).unwrap();
    for (name, device) in &config.memory {
        for mapping in &device.mappings {
            // Prefix with "mem_" to avoid collisions with peripheral register names
            writeln!(
                out,
                "mem_{name}: Memory.MappedMemory @ sysbus {:#010x}",
                mapping.address
            )
            .unwrap();
            writeln!(out, "    size: {:#010x}", mapping.size).unwrap();
            writeln!(out).unwrap();
            break; // only first mapping for Renode
        }
    }

    // Peripherals with Renode models
    writeln!(out, "// Peripherals").unwrap();
    writeln!(out).unwrap();

    let mut silence_ranges = Vec::new();

    for (name, periph) in &config.peripheral_map {
        if let Some(renode) = &periph.renode {
            write!(
                out,
                "{name}: {} @ sysbus {:#010x}",
                renode.model, periph.base
            )
            .unwrap();
            writeln!(out).unwrap();

            for (prop_name, value) in &renode.properties {
                writeln!(out, "    {prop_name}: {value}").unwrap();
            }

            // IRQ connections
            for (irq_name, irq_num) in &periph.irqs {
                let _ = irq_name; // typically just "irq"
                writeln!(out, "    IRQ -> nvic@{irq_num}").unwrap();
            }

            writeln!(out).unwrap();
        } else {
            // No Renode model — silence the register range
            silence_ranges.push((periph.base, periph.size));
        }
    }

    // Init block: silence unmodeled peripherals + pre-configure USART
    writeln!(out, "sysbus:").unwrap();
    writeln!(out, "    init:").unwrap();

    for (base, size) in &silence_ranges {
        writeln!(
            out,
            "        SilenceRange <{:#010x}, {:#010x}>",
            base,
            base + size - 1
        )
        .unwrap();
    }

    out
}
