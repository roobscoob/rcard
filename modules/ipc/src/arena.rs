use crate::RawHandle;

struct HandleEntry {
    key: u64,
    slot: u8,
    generation: u32,
    occupied: bool,
    owner: u16,
}

struct Slot<T> {
    value: Option<T>,
    generation: u32,
}

/// Fixed-size generational arena with opaque handle mapping.
///
/// Externally, handles are opaque `u64` keys. Internally, each key maps
/// to a `(slot_index, generation)` pair. The generation prevents stale
/// handles from resolving after a slot is freed and reused.
pub struct Arena<T, const N: usize> {
    slots: [Slot<T>; N],
    map: [HandleEntry; N],
    next_key: u64,
}

impl<T, const N: usize> Arena<T, N> {
    const EMPTY_SLOT: Slot<T> = Slot {
        value: None,
        generation: 0,
    };

    const EMPTY_ENTRY: HandleEntry = HandleEntry {
        key: 0,
        slot: 0,
        generation: 0,
        occupied: false,
        owner: 0,
    };

    pub const fn new() -> Self {
        assert!(N <= 255, "Arena: N must be <= 255 (slot index is stored as u8)");
        Self {
            slots: [Self::EMPTY_SLOT; N],
            map: [Self::EMPTY_ENTRY; N],
            next_key: 1,
        }
    }

    /// Allocate a new slot, returning a handle to it.
    pub fn alloc(&mut self, value: T, owner: u16) -> Option<RawHandle> {
        let slot_idx = self.slots.iter().position(|s| s.value.is_none())?;
        let map_idx = self.map.iter().position(|e| !e.occupied)?;

        let generation = self.slots[slot_idx].generation;
        self.slots[slot_idx].value = Some(value);

        let key = self.next_key;
        self.next_key = self
            .next_key
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);

        self.map[map_idx] = HandleEntry {
            key,
            slot: slot_idx as u8,
            generation,
            occupied: true,
            owner,
        };

        Some(RawHandle(key))
    }

    fn lookup(&self, handle: RawHandle) -> Option<usize> {
        let entry = self.map.iter().find(|e| e.occupied && e.key == handle.0)?;
        let slot = &self.slots[entry.slot as usize];
        if slot.generation != entry.generation || slot.value.is_none() {
            return None;
        }
        Some(entry.slot as usize)
    }

    pub fn get(&self, handle: RawHandle) -> Option<&T> {
        let idx = self.lookup(handle)?;
        self.slots[idx].value.as_ref()
    }

    pub fn get_mut(&mut self, handle: RawHandle) -> Option<&mut T> {
        let idx = self.lookup(handle)?;
        self.slots[idx].value.as_mut()
    }

    /// Remove a value from the arena, returning it.
    pub fn remove(&mut self, handle: RawHandle) -> Option<T> {
        let entry_idx = self
            .map
            .iter()
            .position(|e| e.occupied && e.key == handle.0)?;
        let slot_idx = self.map[entry_idx].slot as usize;
        let slot = &mut self.slots[slot_idx];

        if slot.generation != self.map[entry_idx].generation || slot.value.is_none() {
            return None;
        }

        let value = slot.value.take();
        slot.generation = slot.generation.wrapping_add(1);
        self.map[entry_idx].occupied = false;
        value
    }

    /// Remove all resources owned by the given task index.
    /// Drops each removed value (triggering `Drop` impls).
    pub fn remove_by_owner(&mut self, task_index: u16) {
        for i in 0..N {
            if self.map[i].occupied && self.map[i].owner == task_index {
                let slot_idx = self.map[i].slot as usize;
                let slot = &mut self.slots[slot_idx];
                if slot.generation == self.map[i].generation {
                    let _ = slot.value.take(); // drops the value
                    slot.generation = slot.generation.wrapping_add(1);
                }
                self.map[i].occupied = false;
            }
        }
    }
}
