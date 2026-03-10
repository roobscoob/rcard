//! FileSystemRegistry resource implementation.
//!
//! Maps 16-byte names to fs_id values (internal slot indices).

use sysmodule_fs_api::{FileSystemRegistry, RegistryError};

use crate::state;

/// Compare two null-padded name buffers, only up to the first null.
fn names_eq(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let a_len = a.iter().position(|&c| c == 0).unwrap_or(a.len());
    let b_len = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    a_len == b_len && a[..a_len] == b[..b_len]
}

/// Register a filesystem by name and fs_id. Used by auto-mount and IPC.
pub fn register_entry(name: [u8; 16], fs_id: u8) -> Result<(), RegistryError> {
    state::with_state(|s| {
        if s.registry
            .iter()
            .any(|e| e.fs_id.is_some() && names_eq(&e.name, &name))
        {
            return Err(RegistryError::AlreadyExists);
        }

        let slot = s
            .registry
            .iter_mut()
            .find(|e| e.fs_id.is_none())
            .ok_or(RegistryError::RegistryFull)?;

        slot.name = name;
        slot.fs_id = Some(fs_id);
        Ok(())
    })
}

/// Look up an fs_id by name.
pub fn lookup_by_name(name: &[u8; 16]) -> Option<u8> {
    state::with_state(|s| {
        s.registry
            .iter()
            .find(|e| e.fs_id.is_some() && names_eq(&e.name, name))
            .and_then(|e| e.fs_id)
    })
}

pub struct RegistryResource;

impl FileSystemRegistry for RegistryResource {
    fn global(_meta: ipc::Meta) -> Self {
        RegistryResource
    }

    fn register(
        &mut self,
        _meta: ipc::Meta,
        name: [u8; 16],
        fs_id: u8,
    ) -> Result<(), RegistryError> {
        register_entry(name, fs_id)
    }
}
