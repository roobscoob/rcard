use std::any::{Any, TypeId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::adapter::AdapterId;

/// Shared state between an [`AdapterLink`] and its [`AdapterHandle`]s.
struct HandleInner {
    adapter_id: AdapterId,
    capabilities: Vec<(TypeId, Arc<dyn Any + Send + Sync>)>,
    revoked: AtomicBool,
}

/// Adapter-side half of the adapter↔device binding.
///
/// Lives in the adapter's connect loop for the lifetime of the physical
/// connection. Call [`handle()`](Self::handle) to mint a new
/// [`AdapterHandle`] each time the device identity changes (reboot,
/// re-enumeration), and [`revoke()`](Self::revoke) to invalidate all
/// outstanding handles when the device disappears.
pub struct AdapterLink {
    inner: Arc<HandleInner>,
}

/// Device-side half of the adapter↔device binding.
///
/// Held by a [`BridgeDevice`](crate::physical::PhysicalDevice). Capability
/// queries return `None` once the adapter has revoked the handle.
#[derive(Clone)]
pub struct AdapterHandle {
    inner: Arc<HandleInner>,
}

impl AdapterLink {
    pub fn new(
        adapter_id: AdapterId,
        capabilities: Vec<(TypeId, Arc<dyn Any + Send + Sync>)>,
    ) -> Self {
        AdapterLink {
            inner: Arc::new(HandleInner {
                adapter_id,
                capabilities,
                revoked: AtomicBool::new(false),
            }),
        }
    }

    pub fn adapter_id(&self) -> AdapterId {
        self.inner.adapter_id
    }

    /// Mint a fresh handle for attachment to a device.
    ///
    /// If a previous handle was revoked, this creates a new inner with
    /// the same capabilities so the new handle starts un-revoked.
    pub fn handle(&mut self) -> AdapterHandle {
        if self.inner.revoked.load(Ordering::SeqCst) {
            self.inner = Arc::new(HandleInner {
                adapter_id: self.inner.adapter_id,
                capabilities: self.inner.capabilities.clone(),
                revoked: AtomicBool::new(false),
            });
        }
        AdapterHandle {
            inner: self.inner.clone(),
        }
    }

    /// Revoke all outstanding handles. Capability queries on existing
    /// handles will return `None` from this point on.
    pub fn revoke(&self) {
        self.inner.revoked.store(true, Ordering::SeqCst);
    }

    pub fn is_revoked(&self) -> bool {
        self.inner.revoked.load(Ordering::SeqCst)
    }
}

impl AdapterHandle {
    pub fn adapter_id(&self) -> AdapterId {
        self.inner.adapter_id
    }

    pub fn is_revoked(&self) -> bool {
        self.inner.revoked.load(Ordering::SeqCst)
    }

    pub fn capabilities(&self) -> &[(TypeId, Arc<dyn Any + Send + Sync>)] {
        if self.is_revoked() {
            &[]
        } else {
            &self.inner.capabilities
        }
    }

    pub fn query(&self, type_id: TypeId) -> Option<Arc<dyn Any + Send + Sync>> {
        if self.is_revoked() {
            return None;
        }
        self.inner
            .capabilities
            .iter()
            .find(|(tid, _)| *tid == type_id)
            .map(|(_, v)| Arc::clone(v))
    }
}
