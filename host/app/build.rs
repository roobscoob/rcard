use std::env;
use std::path::PathBuf;

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
    tfw::build::build(
        &firmware_dir,
        "apps/stub.ncl",
        "boards/bentoboard.ncl",
        "layouts/ramboot.ncl",
        &stub_tfw,
        Some(&|event| {
            use tfw::build::*;
            match &event {
                BuildEvent::Crate { name: _, kind: _, update: ResourceUpdate::Event(event) } => {
                    match event {
                        CrateEvent::CargoMessage(msg) => {
                            if let Ok(decoded) = msg.decode() {
                                if let escargot::format::Message::CompilerMessage(cm) = decoded {
                                    let text = cm.message.rendered
                                        .as_ref()
                                        .map(|s| s.as_ref())
                                        .unwrap_or(cm.message.message.as_ref());
                                    eprint!("{text}");
                                }
                            }
                        }
                        CrateEvent::CargoError(e) => {
                            eprintln!("{e}");
                        }
                        _ => {}
                    }
                }
                BuildEvent::HostCrate { name: _, update: ResourceUpdate::Event(event) } => {
                    match event {
                        HostCrateEvent::CargoMessage(msg) => {
                            if let Ok(decoded) = msg.decode() {
                                if let escargot::format::Message::CompilerMessage(cm) = decoded {
                                    let text = cm.message.rendered
                                        .as_ref()
                                        .map(|s| s.as_ref())
                                        .unwrap_or(cm.message.message.as_ref());
                                    eprint!("{text}");
                                }
                            }
                        }
                        HostCrateEvent::CargoError(e) => {
                            eprintln!("{e}");
                        }
                    }
                }
                _ => {}
            }
        }),
        Some(&stub_work),
    )
    .unwrap_or_else(|e| panic!("stub firmware build failed:\n{e}"));

    eprintln!("stub firmware built: {}", stub_tfw.display());
}
