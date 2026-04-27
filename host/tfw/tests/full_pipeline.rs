use std::path::Path;

fn firmware_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../firmware"))
}

#[test]
fn full_pipeline_config_to_linker() {
    let fw = firmware_dir();
    let work_dir = fw.join(".work");
    std::fs::create_dir_all(&work_dir).unwrap();

    // 1. Config
    let config = tfw::config::load(fw, "fob.ncl", "boards/bentoboard.ncl", "layouts/prod.ncl")
        .expect("config failed");
    assert_eq!(config.name, "rcard");

    // 2. Layout
    let layout = tfw::layout::solve(&config).expect("layout failed");
    assert!(!layout.placed.is_empty());

    // 3. Linker scripts
    let linker_dir = work_dir.join("linker");
    tfw::linker::generate(&config, &layout, &linker_dir).expect("linker failed");

    // Verify kernel linker script
    let kernel_mem = std::fs::read_to_string(linker_dir.join("kernel").join("memory.x")).unwrap();
    assert!(kernel_mem.contains("FLASH"));
    assert!(kernel_mem.contains("STACK"));

    // 4. Codegen JSON
    let config_json = work_dir.join("config.json");
    tfw::codegen::emit(&config, &config_json).expect("codegen failed");

    // Verify config.json
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config_json).unwrap()).unwrap();
    assert!(json["tasks"].as_array().unwrap().len() > 0);
    assert!(json["ipc_acl"].as_object().unwrap().len() > 0);

    // 5. Verify KCONFIG would be generated (don't actually compile)
    let task_names: Vec<&str> = json["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert!(task_names.contains(&"sysmodule_log"));
    assert!(task_names.contains(&"fob"));

    println!("Full pipeline: config -> layout -> linker -> codegen: OK");
    println!("  {} tasks, {} placed regions, {} deferred",
        task_names.len(), layout.placed.len(), layout.deferred.len());
}
