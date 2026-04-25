//! USB-side `FrameSender` impl. The unified `Ipc` capability the device
//! exposes lives in `ipc_protocol`; this struct just owns the bulk-OUT
//! endpoint and turns "send these bytes" into an nusb submission.

use std::future::Future;
use std::pin::Pin;

use nusb::Endpoint;
use nusb::transfer::{Buffer, Bulk, Out};
use tokio::sync::Mutex;

use crate::crc::wrap_frame;

// Re-export the protocol types so existing consumers don't break.
pub use ipc_protocol::{IpcCallResult, IpcError, IpcProtocol, ResolvedResponse};

/// `FrameSender` over a USB bulk-OUT endpoint.
///
/// Mutex-guarded because `Endpoint::submit` / `next_complete` require
/// `&mut self`, and multiple IPC callers may race.
pub struct UsbSender {
    out_endpoint: Mutex<Endpoint<Bulk, Out>>,
}

impl UsbSender {
    pub(crate) fn new(out_endpoint: Endpoint<Bulk, Out>) -> Self {
        Self {
            out_endpoint: Mutex::new(out_endpoint),
        }
    }
}

impl ipc_protocol::FrameSender for UsbSender {
    fn send_frame<'a>(
        &'a self,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            // Append CRC16 (and a pad byte when the wire would otherwise
            // be a multiple of 64, to force a short-packet terminator).
            // Firmware `usb_protocol_host` accumulates packets until the
            // short terminator, validates CRC, and dispatches.
            let wrapped = wrap_frame(&bytes);
            eprintln!(
                "[usb-sender] OUT submit: {} frame bytes, {} wire bytes",
                bytes.len(),
                wrapped.len(),
            );
            let mut buf = Buffer::new(wrapped.len());
            buf.extend_from_slice(&wrapped);
            let mut ep = self.out_endpoint.lock().await;
            ep.submit(buf);
            let completion = ep.next_complete().await;
            match &completion.status {
                Ok(()) => eprintln!("[usb-sender] OUT complete OK"),
                Err(e) => eprintln!("[usb-sender] OUT complete ERROR: {e}"),
            }
            completion.status.map_err(|e| e.to_string())
        })
    }
}
