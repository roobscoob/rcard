mod emulator;
mod serial;

use std::path::PathBuf;

use crate::tfw::{self, ConnectedBackend};

pub async fn run(tfw: PathBuf, backend: String) {
    let backend = tfw::parse_backend(&backend, &tfw);

    match backend {
        ConnectedBackend::Emulator(_) => emulator::unsupported(),
        ConnectedBackend::Serial(ser) => serial::run(ser, &tfw).await,
    }
}
