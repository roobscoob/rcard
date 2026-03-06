//! FileSystemRegistry resource implementation.
//!
//! A simple in-memory table mapping 16-byte names to fs_id values.

use sysmodule_fs_api::{FileSystemRegistry, RegistryError};

const MAX_ENTRIES: usize = 8;

struct RegistryEntry {
    name: [u8; 16],
    fs_id: u8,
    occupied: bool,
}

/// Shared registry state.  Only one registry instance exists (the "global" one).
static mut REGISTRY: [RegistryEntry; MAX_ENTRIES] = {
    const EMPTY: RegistryEntry = RegistryEntry {
        name: [0; 16],
        fs_id: 0,
        occupied: false,
    };
    [EMPTY; MAX_ENTRIES]
};

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
        let reg = unsafe { &mut *core::ptr::addr_of_mut!(REGISTRY) };

        // Check for duplicates.
        if reg.iter().any(|e| e.occupied && e.name == name) {
            return Err(RegistryError::AlreadyExists);
        }

        let slot = reg
            .iter_mut()
            .find(|e| !e.occupied)
            .ok_or(RegistryError::RegistryFull)?;

        slot.name = name;
        slot.fs_id = fs_id;
        slot.occupied = true;
        Ok(())
    }

}
