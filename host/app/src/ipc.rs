//! Re-exports of the unified IPC capability so the rest of the app can
//! refer to it as `crate::ipc::*` without reaching into `ipc-protocol`.
//!
//! The `Ipc` struct itself lives in `ipc-protocol` because each adapter
//! crate (USB, USART2, …) has to construct one and register it as a
//! capability — and those crates can't depend on `host/app`. The single
//! `device.get::<Ipc>()` lookup at the bridge layer is what unifies the
//! transports from the consumer's point of view.

use std::any::TypeId;
use std::sync::Arc;

pub use ipc_protocol::{Ipc, IpcCallResult};

/// Among all `Ipc` capabilities registered on `device`, return the one
/// with the highest `priority()`. Use this instead of `device.get::<Ipc>()`
/// when more than one transport may be present (e.g. USB and USART2 both
/// attached) — the underlying capability registry's `query()` returns
/// whichever was registered first, which depends on adapter attachment
/// order rather than caller intent.
pub fn pick(device: &crate::bridge::BridgeDevice) -> Option<Arc<Ipc>> {
    device
        .query_all_capabilities(TypeId::of::<Ipc>())
        .into_iter()
        .filter_map(|(_, arc): (_, Arc<dyn std::any::Any + Send + Sync>)| arc.downcast::<Ipc>().ok())
        .max_by_key(|i: &Arc<Ipc>| i.priority())
}
