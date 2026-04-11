use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use crate::adapter::AdapterId;

/// Type-erased capability registry.
///
/// Capabilities are stored as `Arc<dyn Any + Send + Sync>`, keyed by `TypeId`,
/// tagged with which adapter provided them. Multiple adapters can provide the
/// same capability type.
pub struct CapabilitySet {
    entries: HashMap<TypeId, Vec<(AdapterId, Arc<dyn Any + Send + Sync>)>>,
}

impl CapabilitySet {
    pub fn new() -> Self {
        CapabilitySet {
            entries: HashMap::new(),
        }
    }

    /// Register a capability provided by an adapter.
    pub fn register<T: Send + Sync + 'static>(&mut self, adapter: AdapterId, val: T) {
        self.entries
            .entry(TypeId::of::<T>())
            .or_default()
            .push((adapter, Arc::new(val)));
    }

    /// Register a pre-typed capability (from [`Adapter::capabilities`]).
    pub fn register_raw(
        &mut self,
        adapter: AdapterId,
        type_id: TypeId,
        value: Arc<dyn Any + Send + Sync>,
    ) {
        self.entries
            .entry(type_id)
            .or_default()
            .push((adapter, value));
    }

    /// Remove all capabilities provided by an adapter.
    pub fn remove_adapter(&mut self, adapter: AdapterId) {
        for vec in self.entries.values_mut() {
            vec.retain(|(id, _)| *id != adapter);
        }
        self.entries.retain(|_, v| !v.is_empty());
    }

    /// Get the first instance of a capability type.
    pub fn query(&self, type_id: TypeId) -> Option<Arc<dyn Any + Send + Sync>> {
        self.entries
            .get(&type_id)
            .and_then(|vec| vec.first())
            .map(|(_, arc)| Arc::clone(arc))
    }

    /// Get all instances of a capability type, with their adapter IDs.
    pub fn query_all(&self, type_id: TypeId) -> Vec<(AdapterId, Arc<dyn Any + Send + Sync>)> {
        self.entries.get(&type_id).cloned().unwrap_or_default()
    }

    /// Check if any adapter provides a capability type.
    pub fn has(&self, type_id: TypeId) -> bool {
        self.entries
            .get(&type_id)
            .is_some_and(|v| !v.is_empty())
    }
}
