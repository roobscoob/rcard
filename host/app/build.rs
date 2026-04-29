use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

fn main() {
    let firmware_dir: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "firmware"]
        .iter()
        .collect();
    let firmware_dir = match firmware_dir.canonicalize() {
        Ok(p) => {
            let s = p.display().to_string();
            PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
        }
        Err(_) => return,
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let stub_tfw = out_dir.join("stub.tfw");

    println!("cargo:rerun-if-changed=nonexistent.trigger");

    let stub_work = out_dir.join("stub_work");
    let log_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".app_build.log");
    let log = Mutex::new(
        std::fs::File::create(&log_path)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", log_path.display())),
    );
    tfw::build::build(
        &firmware_dir,
        "apps/stub.ncl",
        "boards/bentoboard.ncl",
        "layouts/ramboot.ncl",
        &stub_tfw,
        Some(&|event| {
            use tfw::build::*;
            let mut f = log.lock().unwrap();
            match &event {
                BuildEvent::Crate {
                    name,
                    kind,
                    update,
                } => match update {
                    ResourceUpdate::Event(CrateEvent::CargoMessage(msg)) => {
                        if let Ok(decoded) = msg.decode() {
                            let _ = writeln!(f, "[crate:{name}:{kind:?}] {decoded:?}");
                            if let escargot::format::Message::CompilerMessage(cm) = decoded {
                                let text = cm
                                    .message
                                    .rendered
                                    .as_ref()
                                    .map(|s| s.as_ref())
                                    .unwrap_or(cm.message.message.as_ref());
                                eprint!("{text}");
                            }
                        } else {
                            let _ = writeln!(f, "[crate:{name}:{kind:?}] (undecoded message)");
                        }
                    }
                    ResourceUpdate::Event(CrateEvent::CargoError(e)) => {
                        let _ = writeln!(f, "[crate:{name}:{kind:?}] cargo error: {e}");
                        eprintln!("[crate:{name}:{kind:?}] cargo error: {e}");
                    }
                    other => {
                        let _ = writeln!(f, "[crate:{name}:{kind:?}] {other:?}");
                    }
                },
                BuildEvent::HostCrate { name, update } => match update {
                    ResourceUpdate::Event(HostCrateEvent::CargoMessage(msg)) => {
                        if let Ok(decoded) = msg.decode() {
                            let _ = writeln!(f, "[host_crate:{name}] {decoded:?}");
                            if let escargot::format::Message::CompilerMessage(cm) = decoded {
                                let text = cm
                                    .message
                                    .rendered
                                    .as_ref()
                                    .map(|s| s.as_ref())
                                    .unwrap_or(cm.message.message.as_ref());
                                eprint!("{text}");
                            }
                        } else {
                            let _ = writeln!(f, "[host_crate:{name}] (undecoded message)");
                        }
                    }
                    ResourceUpdate::Event(HostCrateEvent::CargoError(e)) => {
                        let _ = writeln!(f, "[host_crate:{name}] cargo error: {e}");
                        eprintln!("[host_crate:{name}] cargo error: {e}");
                    }
                    other => {
                        let _ = writeln!(f, "[host_crate:{name}] {other:?}");
                    }
                },
                BuildEvent::Build(state) => {
                    let _ = writeln!(f, "[build] state -> {state:?}");
                }
                other => {
                    let _ = writeln!(f, "[build] {other:?}");
                }
            }
            let _ = f.flush();
        }),
        Some(&stub_work),
    )
    .unwrap_or_else(|e| panic!("stub firmware build failed:\n{e}"));

    eprintln!("stub firmware built: {}", stub_tfw.display());
}
