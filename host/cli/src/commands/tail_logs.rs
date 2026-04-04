use std::path::PathBuf;

use engine::Backend;

use crate::format::{self, prefix_width};
use crate::tfw::{self, ConnectedBackend, TfwMetadata};

pub async fn run(tfw: PathBuf, backend: String) {
    let meta = tfw::load_metadata(&tfw);
    let backend = tfw::parse_backend(&backend, &tfw);

    println!("\x1b[1mTailing logs from backend for firmware {tfw:?}:\x1b[0m");

    tail_logs(&backend, &meta).await;
}

async fn tail_logs(backend: &ConnectedBackend, meta: &TfwMetadata) {
    let logs = backend.logs();

    let mut structured_rx = logs.subscribe_structured();
    let mut usart1_rx = logs.subscribe_usart1();

    // Merge all auxiliary streams into a single channel.
    let (aux_tx, mut aux_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    for name in logs.auxiliary_streams() {
        if let Some(mut rx) = logs.subscribe_auxiliary(name) {
            let name = name.to_string();
            let tx = aux_tx.clone();
            tokio::spawn(async move {
                while let Ok(text) = rx.recv().await {
                    if tx.send((name.clone(), text)).is_err() {
                        break;
                    }
                }
            });
        }
    }
    drop(aux_tx); // so aux_rx closes when all forwarders are done

    let task_pad = meta
        .task_names
        .iter()
        .map(|n| n.strip_prefix("sysmodule_").unwrap_or(n).len())
        .max()
        .unwrap_or(0)
        .max(6);
    let pw = prefix_width(task_pad);

    loop {
        tokio::select! {
            Ok(entry) = structured_rx.recv() => {
                format::print_structured(&entry, meta, task_pad, pw);
            }
            Ok(line) = usart1_rx.recv() => {
                format::print_text_line("usart1", &line.text, task_pad);
            }
            Some((name, text)) = aux_rx.recv() => {
                format::print_text_line(&name, &text, task_pad);
            }
            else => break,
        }
    }
}
