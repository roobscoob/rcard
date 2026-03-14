use crate::RawHandle;

/// A type-erased handle that can point to any server implementing a given
/// interface. Carries the server's TaskId (encoded as raw u16), resource kind,
/// and raw handle.
///
/// Used on the wire when passing `impl Trait` handle parameters. The recipient
/// uses `server_id` + `kind` to route IPC calls to the correct server.
#[derive(
    Copy, Clone, Debug,
    zerocopy::FromBytes, zerocopy::IntoBytes,
    zerocopy::KnownLayout, zerocopy::Immutable,
)]
#[repr(C, packed)]
pub struct DynHandle {
    /// Raw `TaskId` value (encodes both task index and generation).
    pub server_id: u16,
    pub kind: u8,
    pub handle: RawHandle,
}

impl DynHandle {
    /// Extract the task index from the stored server ID.
    pub fn task_index(&self) -> u16 {
        crate::kern::TaskId::from(self.server_id).task_index()
    }

    /// Reconstruct the `TaskId` from the stored raw value.
    pub fn task_id(&self) -> crate::kern::TaskId {
        crate::kern::TaskId::from(self.server_id)
    }
}
