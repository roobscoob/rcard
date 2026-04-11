use std::path::Path;

#[test]
fn emit_fob_config_json() {
    let fw = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../firmware"));
    let work_dir = fw.join(".work");
    std::fs::create_dir_all(&work_dir).unwrap();

    let config = tfw::config::load(fw, "fob.ncl", "boards/bentoboard.ncl", "layouts/prod.ncl")
        .expect("config load failed");

    let config_json = work_dir.join("config.json");
    tfw::codegen::emit(&config, &config_json).expect("codegen failed");
}
