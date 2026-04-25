use std::any::{Any, TypeId};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::broadcast;

use crate::adapter::{Adapter, AdapterId};
use crate::logs::{ControlEvent, Log, LogContents, LogEntry};

/// Events emitted by a device — adapter lifecycle changes, logs, and errors.
#[derive(Clone, Debug)]
pub enum DeviceEvent {
    AdapterConnected(AdapterId),
    AdapterDisconnected(AdapterId),
    Log(Log),
    /// A non-log control event observed on an adapter (e.g. an IPC reply
    /// or tunnel error seen on the USART2 channel).
    Control {
        adapter: AdapterId,
        event: ControlEvent,
    },
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
    ///
    /// Stamped with the current instant; use [`structured_at`] to preserve
    /// the first-byte-received time from an earlier point in the pipeline.
    pub fn structured(&self, entry: LogEntry) {
        self.structured_at(entry, Instant::now());
    }

    /// Send a structured log entry with an explicit `received_at` stamp.
    pub fn structured_at(&self, entry: LogEntry, received_at: Instant) {
        let device_tick = Some(entry.timestamp);
        let _ = self.tx.send(DeviceEvent::Log(Log {
            adapter: self.adapter,
            contents: LogContents::Structured(entry),
            received_at,
            device_tick,
        }));
    }

    /// Send a text log line (from a text stream like USART1).
    pub fn text(&self, text: String) {
        self.text_at(text, Instant::now());
    }

    /// Send a text log line with an explicit `received_at` stamp.
    pub fn text_at(&self, text: String, received_at: Instant) {
        let _ = self.tx.send(DeviceEvent::Log(Log {
            adapter: self.adapter,
            contents: LogContents::Text(text),
            received_at,
            device_tick: None,
        }));
    }

    /// Send a named auxiliary text log (e.g. "renode").
    pub fn auxiliary(&self, name: String, text: String) {
        self.auxiliary_at(name, text, Instant::now());
    }

    /// Send a named auxiliary text log with an explicit `received_at` stamp.
    pub fn auxiliary_at(&self, name: String, text: String, received_at: Instant) {
        let _ = self.tx.send(DeviceEvent::Log(Log {
            adapter: self.adapter,
            contents: LogContents::Auxiliary { name, text },
            received_at,
            device_tick: None,
        }));
    }

    /// Send a Renode emulator log with a parsed level and message.
    pub fn renode(&self, level: rcard_log::LogLevel, message: String) {
        self.renode_at(level, message, Instant::now());
    }

    /// Send a Renode emulator log with an explicit `received_at` stamp.
    pub fn renode_at(&self, level: rcard_log::LogLevel, message: String, received_at: Instant) {
        let _ = self.tx.send(DeviceEvent::Log(Log {
            adapter: self.adapter,
            contents: LogContents::Renode { level, message },
            received_at,
            device_tick: None,
        }));
    }

    /// Send a non-log control event (e.g. an IPC reply decoded off USART2).
    pub fn control(&self, event: ControlEvent) {
        let _ = self.tx.send(DeviceEvent::Control {
            adapter: self.adapter,
            event,
        });
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

    /// Attach an adapter after the device has been created. Registers
    /// its capabilities and emits `AdapterConnected`. Default impl is a
    /// no-op for device kinds that don't support dynamic attachment
    /// (e.g. emulators with a fixed adapter set).
    fn attach_adapter(&mut self, _adapter: Box<dyn Adapter>) {}

    /// Detach an adapter by id. Removes its capabilities and emits
    /// `AdapterDisconnected`. Default impl is a no-op.
    fn detach_adapter(&mut self, _id: AdapterId) {}

    /// Hand out a `LogSink` bound to this device's broadcast channel,
    /// tagged with the given `AdapterId`. Returns `None` if the device
    /// doesn't expose a sink (e.g. emulator-internal devices).
    fn log_sink(&self, _adapter: AdapterId) -> Option<LogSink> {
        None
    }
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
