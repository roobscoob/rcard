use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // The tfw build system injects the firmware partition location via
    // HUBRIS_KCONFIG as "addr,size" (e.g. "0x12009000_u32,0x5c000_u32").
    if let Ok(kconfig) = std::env::var("HUBRIS_KCONFIG") {
        let parts: Vec<&str> = kconfig.split(',').collect();
        std::fs::write(out.join("firmware_addr.rs"), parts[0]).unwrap();
        std::fs::write(out.join("firmware_size.rs"), parts[1]).unwrap();
    } else {
        // Defaults for IDE / cargo check builds.
        std::fs::write(out.join("firmware_addr.rs"), "0x12009000_u32").unwrap();
        std::fs::write(out.join("firmware_size.rs"), "0x5c000_u32").unwrap();
    }
}
