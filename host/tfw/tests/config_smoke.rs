use std::path::Path;

fn firmware_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../firmware"))
}

#[test]
fn load_fob_config() {
    let config = tfw::config::load(
        firmware_dir(), "apps/fob.ncl", "boards/bentoboard.ncl", "layouts/prod.ncl",
    ).expect("failed to load fob config");

    assert_eq!(config.name, "rcard");
    assert_eq!(config.target, "thumbv8m.main-none-eabihf");

    // Memory devices
    assert!(config.memory.contains_key("sram_fast_dctm"));
    assert!(config.memory.contains_key("sram_bulk"));
    assert!(config.memory.contains_key("mpi1"));
    assert!(config.memory.contains_key("mpi2"));

    // Places from layout
    assert!(config.places.contains_key("kernel_ram"));
    assert!(config.places.contains_key("task_ram"));
    assert!(config.places.contains_key("image"));
    assert!(config.places.contains_key("logs"));
    assert!(config.places.contains_key("framebuf"));

    // Unmapped places
    assert!(config.places["logs"].unmapped);
    assert!(!config.places["image"].unmapped);

    // Bootloader
    let bl = config.bootloader.as_ref().expect("bootloader should be present");
    assert_eq!(bl.crate_info.package.name, "bootloader-sf32lb52");
    assert!(bl.regions.contains_key("code"));
    assert!(bl.regions.contains_key("stack"));

    // Boot config has ftab placement
    let boot = config.boot.as_ref().expect("boot config should exist");
    assert!(boot.ftab.offset.is_some());

    // Kernel
    assert_eq!(config.kernel.crate_info.package.name, "kernel-sf32lb52");

    // Task regions carry inline places
    let fob = config.entries.iter().find(|t| t.crate_info.package.name == "fob").unwrap();
    assert!(!fob.regions["code"].place.mappings.is_empty());
}

#[test]
fn load_stub_config() {
    let config = tfw::config::load(
        firmware_dir(), "apps/stub.ncl", "boards/bentoboard.ncl", "layouts/prod.ncl",
    ).expect("failed to load stub config");

    let entry_crates: Vec<&str> = config
        .entries
        .iter()
        .map(|t| t.crate_info.package.name.as_str())
        .collect();
    assert!(entry_crates.contains(&"stub"));
    assert!(!entry_crates.contains(&"fob"));
}

#[test]
fn task_discovery_excludes_kernel() {
    let tasks = tfw::config::discover_tasks(firmware_dir());
    assert!(!tasks.contains_key("kernel-sf32lb52"));
    assert!(tasks.contains_key("fob"));
    assert!(tasks.contains_key("sysmodule_log"));
}
