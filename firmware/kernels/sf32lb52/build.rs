#![allow(clippy::unwrap_used)]

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Provide device.x with DefaultHandler entries for all SF32LB52 IRQs.
    // The PAC's __INTERRUPTS is [Vector; 0] (svd2rust didn't extract IRQs),
    // so we must provide the vector table ourselves.
    // SF32LB52 has IRQs up to 98, so we need 99 entries.
    let mut device_x = String::new();
    for i in 0..99 {
        device_x.push_str(&format!("PROVIDE(Interrupt{} = DefaultHandler);\n", i));
    }
    fs::write(out.join("device.x"), &device_x).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
}