use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::adapter::{Adapter, AdapterId};
use crate::capability::CapabilitySet;
use crate::device::{Device, DeviceEvent, LogSink};

/// A physical device with independently-managed adapters.
///
/// Adapters can be attached and detached at any time. Each adapter contributes
/// capabilities and may push log events.
pub struct PhysicalDevice {
    events_tx: broadcast::Sender<DeviceEvent>,
    capabilities: CapabilitySet,
    adapters: HashMap<AdapterId, Box<dyn Adapter>>,
    next_adapter_id: u64,
}

impl PhysicalDevice {
    pub fn new() -> Self {
        let (events_tx, _) = broadcast::channel(256);
        PhysicalDevice {
            events_tx,
            capabilities: CapabilitySet::new(),
            adapters: HashMap::new(),
            next_adapter_id: 0,
        }
    }

    /// Allocate the next adapter ID.
    pub fn next_adapter_id(&mut self) -> AdapterId {
        let id = AdapterId(self.next_adapter_id);
        self.next_adapter_id += 1;
        id
    }

    /// Create a LogSink for an adapter to push events into this device.
    pub fn log_sink(&self, adapter: AdapterId) -> LogSink {
        LogSink::new(adapter, self.events_tx.clone())
    }

    /// Attach an adapter. Registers its capabilities and emits AdapterConnected.
    pub fn attach(&mut self, adapter: impl Adapter) {
        self.attach_boxed(Box::new(adapter));
    }

    /// Boxed variant — used when attaching through the `Device` trait
    /// object where the concrete adapter type has been erased.
    pub fn attach_boxed(&mut self, adapter: Box<dyn Adapter>) {
        let id = adapter.id();
        for (type_id, value) in adapter.capabilities() {
            self.capabilities.register_raw(id, type_id, value);
        }
        self.adapters.insert(id, adapter);
        let _ = self.events_tx.send(DeviceEvent::AdapterConnected(id));
    }

    /// Detach an adapter. Removes its capabilities and emits AdapterDisconnected.
    pub fn detach(&mut self, id: AdapterId) {
        self.capabilities.remove_adapter(id);
        self.adapters.remove(&id);
        let _ = self.events_tx.send(DeviceEvent::AdapterDisconnected(id));
    }
}

impl Device for PhysicalDevice {
    fn subscribe(&self) -> broadcast::Receiver<DeviceEvent> {
        self.events_tx.subscribe()
    }

    fn attach_adapter(&mut self, adapter: Box<dyn Adapter>) {
        self.attach_boxed(adapter);
    }

    fn detach_adapter(&mut self, id: AdapterId) {
        self.detach(id);
    }

    fn log_sink(&self, adapter: AdapterId) -> Option<LogSink> {
        Some(PhysicalDevice::log_sink(self, adapter))
    }

    fn query_capability(&self, type_id: TypeId) -> Option<Arc<dyn Any + Send + Sync>> {
        self.capabilities.query(type_id)
    }

    fn query_all_capabilities(
        &self,
        type_id: TypeId,
    ) -> Vec<(AdapterId, Arc<dyn Any + Send + Sync>)> {
        self.capabilities.query_all(type_id)
    }

    fn has_capability(&self, type_id: TypeId) -> bool {
        self.capabilities.has(type_id)
    }

    fn adapters(&self) -> Vec<(AdapterId, &dyn Adapter)> {
        self.adapters
            .iter()
            .map(|(id, adapter)| (*id, adapter.as_ref() as &dyn Adapter))
            .collect()
    }
}
