use std::env;
use std::path::PathBuf;
use std::process::Command;

fn board_name() -> &'static str {
    let count = cfg!(feature = "board-devkit-nano") as u8
        + cfg!(feature = "board-devkit-lcd") as u8
        + cfg!(feature = "board-bentoboard") as u8;

    if count > 1 {
        panic!("multiple board features selected — enable exactly one");
    }

    if cfg!(feature = "board-devkit-nano") {
        "sf32lb52-devkit-nano"
    } else if cfg!(feature = "board-devkit-lcd") {
        "sf32lb52-devkit-lcd"
    } else if cfg!(feature = "board-bentoboard") {
        "bentoboard"
    } else {
        panic!(
            "no board selected — enable one of: board-devkit-nano, board-devkit-lcd, board-bentoboard\n\
             e.g. cargo build --features board-bentoboard"
        );
    }
}

fn main() {
    let firmware_dir: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "firmware"]
        .iter()
        .collect();
    let firmware_dir = firmware_dir
        .canonicalize()
        .expect("firmware directory not found");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let stub_tfw = out_dir.join("stub.tfw");

    let board = board_name();
    let mut cmd = Command::new("nu");
    cmd.arg(firmware_dir.join("build.nu"))
        .args(["--features", "stub"])
        .args(["--board", board])
        .args(["--code-target", "ram@[0..384k]", "--ram-target", "ram@[384k..]"])
        .arg("--out")
        .arg(&stub_tfw)
        .current_dir(&firmware_dir);

    // Remove Cargo build-script env vars so the nested firmware build
    // doesn't inherit the host build's configuration and target dir.
    for (key, _) in env::vars() {
        if key.starts_with("CARGO") || key.starts_with("DEP_") {
            cmd.env_remove(&key);
        }
    }
    for key in [
        "TARGET",
        "HOST",
        "OPT_LEVEL",
        "PROFILE",
        "OUT_DIR",
        "DEBUG",
        "NUM_JOBS",
        "RUSTUP_TOOLCHAIN",
        "RUSTC",
        "RUSTDOC",
        "RUSTC_LINKER",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
    ] {
        cmd.env_remove(key);
    }

    let status = cmd
        .status()
        .expect("failed to run `nu` — is nushell installed?");

    if !status.success() {
        panic!("firmware stub build failed (exit {})", status);
    }

    // Rebuild when the firmware source changes.
    println!("cargo:rerun-if-changed={}", firmware_dir.display());
}
