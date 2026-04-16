use nusb::MaybeFuture;

fn main() {
    let devices = nusb::list_devices().wait().unwrap();
    for d in devices {
        if d.vendor_id() != 0x16D0 || d.product_id() != 0x14EF {
            continue;
        }
        println!(
            "device vid=0x{:04x} pid=0x{:04x} serial={:?}",
            d.vendor_id(),
            d.product_id(),
            d.serial_number(),
        );
        println!("  interfaces (from DeviceInfo):");
        for i in d.interfaces() {
            println!(
                "    iface#{} class={:#x} subclass={:#x} protocol={:#x} str={:?}",
                i.interface_number(),
                i.class(),
                i.subclass(),
                i.protocol(),
                i.interface_string(),
            );
        }

        let dev = match d.open().wait() {
            Ok(d) => d,
            Err(e) => {
                println!("  open failed: {e}");
                continue;
            }
        };
        let active = match dev.active_configuration() {
            Ok(c) => c,
            Err(e) => {
                println!("  active_configuration failed: {e}");
                continue;
            }
        };
        println!(
            "  active config value={} num_interfaces={}",
            active.configuration_value(),
            active.num_interfaces(),
        );
        for alt in active.interface_alt_settings() {
            println!(
                "    alt iface#{} setting={} num_endpoints={} class={:#x}/{:#x}/{:#x}",
                alt.interface_number(),
                alt.alternate_setting(),
                alt.num_endpoints(),
                alt.class(),
                alt.subclass(),
                alt.protocol(),
            );
            for ep in alt.endpoints() {
                println!(
                    "      ep addr=0x{:02x} dir={:?} transfer={:?} max_packet={}",
                    ep.address(),
                    ep.direction(),
                    ep.transfer_type(),
                    ep.max_packet_size(),
                );
            }
        }
    }
}
