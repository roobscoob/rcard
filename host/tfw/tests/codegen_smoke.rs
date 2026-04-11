use std::path::Path;

fn firmware_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../firmware"))
}

#[test]
fn emit_config_json() {
    let config = tfw::config::load(
        firmware_dir(), "fob.ncl", "boards/bentoboard.ncl", "layouts/prod.ncl",
    ).expect("failed to load config");

    let out_path = std::env::temp_dir().join("tfw_codegen_test").join("config.json");
    let _ = std::fs::remove_dir_all(out_path.parent().unwrap());

    tfw::codegen::emit(&config, &out_path).expect("codegen failed");

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();

    let tasks = json["tasks"].as_array().unwrap();
    assert!(!tasks.is_empty());
    assert!(tasks.iter().any(|t| t.as_str() == Some("fob")));
    assert!(tasks.iter().any(|t| t.as_str() == Some("sysmodule_log")));
    assert!(!tasks.iter().any(|t| t.as_str() == Some("stub")));

    let _ = std::fs::remove_dir_all(out_path.parent().unwrap());
}
