use std::path::Path;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(hubris_target, values(any()))");
    let target = std::env::var("TARGET").unwrap();
    println!("cargo::rerun-if-env-changed=TARGET");
    println!("cargo::rustc-cfg=hubris_target=\"{target}\"");

    // It's surprisingly hard to get the package version as a _byte string_ at
    // compile time, so, we'll do it the hard way.
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let n = version.len();
    let versionfile = format!("
        #[link_section = \".hubris_abi_version\"]
        #[used]
        static HUBRIS_API_VERSION: [u8; {n}+1] = *b\"{version}\0\";
        ");
    let outdir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(
        Path::new(&outdir).join("hubris_abi_version.rs"),
        versionfile,
    ).unwrap();
}
