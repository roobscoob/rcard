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

    // Always rerun — the real dependencies live in the firmware/ tree and are
    // hard to enumerate. The inner cargo build is cached independently.
    println!("cargo:rerun-if-changed=nonexistent.trigger");

    let stub_work = out_dir.join("stub_work");
    tfw::build::build(
        &firmware_dir,
        "apps/stub.ncl",
        "boards/bentoboard.ncl",
        "layouts/ramboot.ncl",
        &stub_tfw,
        Some(&|event| match &event {
            tfw::build::BuildEvent::CargoMessage(msg) => {
                if let Ok(decoded) = msg.decode() {
                    if let escargot::format::Message::CompilerMessage(cm) = decoded {
                        // Prefer the fully-rendered compiler output (with
                        // file/line/span/help text) so build.rs panics
                        // surface usable diagnostics. Fall back to the
                        // short message if rendering is unavailable.
                        if let Some(rendered) = &cm.message.rendered {
                            eprint!("{rendered}");
                        } else {
                            eprintln!("{}", cm.message.message);
                        }
                    }
                }
            }
            tfw::build::BuildEvent::RegionMeasured { task, region, size } => {
                eprintln!("[size] {task}.{region} = {size} bytes");
            }
            _ => {}
        }),
        Some(&stub_work),
    )
    .unwrap_or_else(|e| panic!("stub firmware build failed:\n{e}"));

    eprintln!("stub firmware built: {}", stub_tfw.display());
}
