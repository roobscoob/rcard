use std::path::Path;

fn firmware_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../firmware"))
}

#[test]
fn full_config_to_codegen_roundtrip() {
    let fw = firmware_dir();
    let work_dir = fw.join(".work");
    std::fs::create_dir_all(&work_dir).unwrap();

    let config = tfw::config::load(fw, "stub.ncl", "boards/bentoboard.ncl", "layouts/prod.ncl")
        .expect("config load failed");

    let layout = tfw::layout::solve(&config).expect("layout failed");

    let linker_dir = work_dir.join("linker");
    tfw::linker::generate(&config, &layout, &linker_dir)
        .expect("linker gen failed");

    let config_json = work_dir.join("config.json");
    tfw::codegen::emit(&config, &config_json).expect("codegen failed");

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_json).unwrap()).unwrap();
    let tasks = json["tasks"].as_array().unwrap();

    assert!(tasks.iter().any(|t| t.as_str() == Some("sysmodule_usb")));
    assert!(!tasks.iter().any(|t| t.as_str() == Some("sysmodule_compositor")));
}
