use std::any::{Any, TypeId};
use std::sync::Arc;

/// Unique identifier for a connected adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AdapterId(pub u64);

/// Something that can be plugged into a device to provide capabilities and log streams.
///
/// Adapters are RAII — dropping an adapter should stop its background tasks.
pub trait Adapter: Send + Sync + 'static {
    /// The unique ID assigned to this adapter.
    fn id(&self) -> AdapterId;

    /// Human-readable name for display in UI.
    fn display_name(&self) -> &str;

    /// Capabilities this adapter provides.
    /// Each entry is a `(TypeId, value)` pair. The values are registered in the
    /// device's [`CapabilitySet`](crate::capability::CapabilitySet) when the
    /// adapter is attached.
    fn capabilities(&self) -> Vec<(TypeId, Arc<dyn Any + Send + Sync>)>;
}
