use std::path::Path;

fn firmware_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../firmware"))
}

#[test]
fn generate_linker_scripts() {
    let config = tfw::config::load(
        firmware_dir(), "fob.ncl", "boards/bentoboard.ncl", "layouts/prod.ncl",
    ).expect("failed to load config");
    let layout = tfw::layout::solve(&config).expect("layout failed");

    let out_dir = std::env::temp_dir().join("tfw_linker_test");
    let _ = std::fs::remove_dir_all(&out_dir);

    tfw::linker::generate(&config, &layout, &out_dir).expect("linker gen failed");

    let kernel_mem = std::fs::read_to_string(out_dir.join("kernel").join("memory.x")).unwrap();
    let kernel_link = std::fs::read_to_string(out_dir.join("kernel").join("link.x")).unwrap();

    assert!(kernel_mem.contains("MEMORY"));
    assert!(kernel_mem.contains("FLASH"));
    assert!(kernel_mem.contains("STACK"));
    assert!(kernel_link.contains("ENTRY(Reset)"));

    // Bootloader linker scripts
    let bl_mem = std::fs::read_to_string(out_dir.join("bootloader").join("memory.x")).unwrap();
    let bl_link = std::fs::read_to_string(out_dir.join("bootloader").join("link.x")).unwrap();
    assert!(bl_mem.contains("FLASH"));
    assert!(bl_mem.contains("STACK"));
    assert!(bl_link.contains("ENTRY(_start)"));

    let _ = std::fs::remove_dir_all(&out_dir);
}
