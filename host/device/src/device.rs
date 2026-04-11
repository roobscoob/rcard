use std::any::{Any, TypeId};
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::adapter::{Adapter, AdapterId};
use crate::logs::{Log, LogContents, LogEntry};

/// Events emitted by a device — adapter lifecycle changes, logs, and errors.
#[derive(Clone, Debug)]
pub enum DeviceEvent {
    AdapterConnected(AdapterId),
    AdapterDisconnected(AdapterId),
    Log(Log),
    Error(AdapterError),
}

/// An error from an adapter, carrying the adapter's own structured error type.
///
/// The UI can display the error generically via `Display`, or downcast
/// `error` to the adapter's concrete error type (e.g. `UsbError`,
/// `SerialError`) for specific handling.
#[derive(Clone, Debug)]
pub struct AdapterError {
    pub adapter: AdapterId,
    pub error: Arc<dyn std::error::Error + Send + Sync>,
}

/// Clonable handle for adapters to push log events into a device.
///
/// Tagged with the adapter's ID so logs carry their source automatically.
#[derive(Clone)]
pub struct LogSink {
    adapter: AdapterId,
    tx: broadcast::Sender<DeviceEvent>,
}

impl LogSink {
    pub fn new(adapter: AdapterId, tx: broadcast::Sender<DeviceEvent>) -> Self {
        LogSink { adapter, tx }
    }

    /// Send a structured log entry (from a binary stream like USART2).
    pub fn structured(&self, entry: LogEntry) {
        let _ = self.tx.send(DeviceEvent::Log(Log {
            adapter: self.adapter,
            contents: LogContents::Structured(entry),
        }));
    }

    /// Send a text log line (from a text stream like USART1).
    pub fn text(&self, text: String) {
        let _ = self.tx.send(DeviceEvent::Log(Log {
            adapter: self.adapter,
            contents: LogContents::Text(text),
        }));
    }

    /// Send a named auxiliary text log (e.g. "renode").
    pub fn auxiliary(&self, name: String, text: String) {
        let _ = self.tx.send(DeviceEvent::Log(Log {
            adapter: self.adapter,
            contents: LogContents::Auxiliary { name, text },
        }));
    }

    /// Send a Renode emulator log with a parsed level and message.
    pub fn renode(&self, level: rcard_log::LogLevel, message: String) {
        let _ = self.tx.send(DeviceEvent::Log(Log {
            adapter: self.adapter,
            contents: LogContents::Renode { level, message },
        }));
    }

    /// Report an adapter error.
    pub fn error(&self, error: impl std::error::Error + Send + Sync + 'static) {
        let _ = self.tx.send(DeviceEvent::Error(AdapterError {
            adapter: self.adapter,
            error: Arc::new(error),
        }));
    }
}

/// A device — physical or emulated — that provides capabilities and emits events.
///
/// Object-safe. Use [`DeviceExt`] for ergonomic typed capability queries.
pub trait Device: Send + Sync {
    /// Subscribe to all device events (adapter changes + logs).
    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent>;

    /// Query a capability by TypeId. Returns the first match.
    fn query_capability(&self, type_id: TypeId) -> Option<Arc<dyn Any + Send + Sync>>;

    /// Query all instances of a capability type, with their adapter IDs.
    fn query_all_capabilities(
        &self,
        type_id: TypeId,
    ) -> Vec<(AdapterId, Arc<dyn Any + Send + Sync>)>;

    /// Check if any adapter provides a capability type.
    fn has_capability(&self, type_id: TypeId) -> bool;

    /// List connected adapters.
    fn adapters(&self) -> Vec<(AdapterId, &dyn Adapter)>;
}

/// Extension trait for ergonomic typed capability queries.
///
/// ```ignore
/// if let Some(debug) = device.get::<DebugMemory>() {
///     debug.handle.mem_read(0x2000_0000, 4).await?;
/// }
/// ```
pub trait DeviceExt: Device {
    fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.query_capability(TypeId::of::<T>())
            .and_then(|arc| arc.downcast::<T>().ok())
    }

    fn has<T: Send + Sync + 'static>(&self) -> bool {
        self.has_capability(TypeId::of::<T>())
    }
}

impl<D: Device + ?Sized> DeviceExt for D {}
