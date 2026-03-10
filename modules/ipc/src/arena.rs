use crate::RawHandle;

/// Error returned by `Arena::clone_handle`.
#[derive(Debug)]
pub enum CloneError {
    /// The source handle doesn't exist or isn't owned by the caller.
    InvalidHandle,
    /// The map has no free entries or the refcount would overflow.
    ArenaFull,
}

struct HandleEntry {
    key: u64,
    slot: u8,
    generation: u32,
    occupied: bool,
    owner: u16,
    priority: i8,
}

struct Slot<T> {
    value: Option<T>,
    generation: u32,
    refcount: u16,
}

/// Fixed-size generational arena with opaque handle mapping.
///
/// Externally, handles are opaque `u64` keys. Internally, each key maps
/// to a `(slot_index, generation)` pair. The generation prevents stale
/// handles from resolving after a slot is freed and reused.
///
/// Supports refcounting: multiple `HandleEntry` rows can point to the same
/// slot. The slot's value is only dropped when the last reference is removed.
pub struct Arena<T, const N: usize> {
    slots: [Slot<T>; N],
    map: [HandleEntry; N],
    // TODO: Replace with hardware PRNG when available. Currently a
    // deterministic LCG — handle keys are predictable across servers.
    next_key: u64,
}

impl<T, const N: usize> Arena<T, N> {
    const EMPTY_SLOT: Slot<T> = Slot {
        value: None,
        generation: 0,
        refcount: 0,
    };

    const EMPTY_ENTRY: HandleEntry = HandleEntry {
        key: 0,
        slot: 0,
        generation: 0,
        occupied: false,
        owner: 0,
        priority: 0,
    };

    pub const fn new(kind: u8) -> Self {
        assert!(N <= 255, "Arena: N must be <= 255 (slot index is stored as u8)");
        Self {
            slots: [Self::EMPTY_SLOT; N],
            map: [Self::EMPTY_ENTRY; N],
            // Seed with kind byte so different arenas produce different key sequences.
            next_key: (kind as u64) << 56 | 1,
        }
    }

    fn next_key(&mut self) -> u64 {
        let key = self.next_key;
        self.next_key = self
            .next_key
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        key
    }

    /// Allocate a new slot, returning a handle to it.
    ///
    /// If the arena is full and `priority` is strictly greater than the
    /// lowest-priority occupied entry, that entry is evicted (its value is
    /// dropped and its generation is bumped) to make room.
    pub fn alloc(&mut self, value: T, owner: u16, priority: i8) -> Option<RawHandle> {
        // Find a free slot, or evict an entire slot to free one.
        let slot_idx = match self.slots.iter().position(|s| s.value.is_none()) {
            Some(idx) => idx,
            None => self.evict_slot(priority)?,
        };

        // evict_slot releases all map entries for the victim slot, so at
        // least one free map entry must exist. If none existed before
        // eviction (no free slots implies no free map entries for
        // non-refcounted resources), the eviction freed at least one.
        let map_idx = self
            .map
            .iter()
            .position(|e| !e.occupied)
            .expect("ipc: arena has a free slot but no free map entry");

        let generation = self.slots[slot_idx].generation;
        self.slots[slot_idx].value = Some(value);
        self.slots[slot_idx].refcount = 1;

        let key = self.next_key();

        self.map[map_idx] = HandleEntry {
            key,
            slot: slot_idx as u8,
            generation,
            occupied: true,
            owner,
            priority,
        };

        Some(RawHandle(key))
    }

    /// Evict an entire slot to free it for a new allocation.
    ///
    /// Eligible slots are those whose **max entry priority** is strictly below
    /// `requester_priority`. Among eligible slots, the one with the lowest
    /// refcount is chosen (least collateral damage). All map entries pointing
    /// to the chosen slot are released, dropping the value and bumping the
    /// generation.
    ///
    /// Returns the freed slot index, or `None` if no slot is evictable.
    fn evict_slot(&mut self, requester_priority: i8) -> Option<usize> {
        // Pass 1: for each occupied slot, compute max priority across its
        // map entries. Only consider slots where max_priority < requester.
        let mut best: Option<(u8, i8, u16)> = None; // (slot_idx, max_priority, refcount)
        for i in 0..N {
            if self.slots[i].value.is_none() {
                continue;
            }
            let slot_idx = i as u8;
            let mut max_prio = i8::MIN;
            for j in 0..N {
                if self.map[j].occupied && self.map[j].slot == slot_idx
                    && self.map[j].generation == self.slots[i].generation
                {
                    if self.map[j].priority > max_prio {
                        max_prio = self.map[j].priority;
                    }
                }
            }
            if max_prio >= requester_priority {
                continue;
            }
            let refcount = self.slots[i].refcount;
            let dominated = match best {
                None => true,
                Some((_, bp, br)) => {
                    // Prefer lower refcount (less damage); break ties by
                    // lower max priority (easier to justify eviction).
                    refcount < br || (refcount == br && max_prio < bp)
                }
            };
            if dominated {
                best = Some((slot_idx, max_prio, refcount));
            }
        }
        let (victim_slot, _, _) = best?;

        // Pass 2: release all map entries pointing to the victim slot.
        for i in 0..N {
            if self.map[i].occupied && self.map[i].slot == victim_slot
                && self.map[i].generation == self.slots[victim_slot as usize].generation
            {
                self.release_entry(i);
            }
        }

        // The slot must now be free.
        debug_assert!(self.slots[victim_slot as usize].value.is_none());
        Some(victim_slot as usize)
    }

    /// Find the map index of the lowest-priority occupied entry whose priority
    /// is strictly less than `requester_priority`. Returns `None` if no such
    /// entry exists (all entries are >= requester priority).
    ///
    /// Used by `clone_handle` which only needs a free map entry, not a free slot.
    /// `exclude_slot` prevents eviction of entries pointing to a specific slot
    /// (used to protect the source slot during clone).
    fn find_eviction_victim(
        &self,
        requester_priority: i8,
        exclude_slot: Option<(u8, u32)>,
    ) -> Option<usize> {
        let mut best: Option<(usize, i8)> = None;
        for i in 0..N {
            if self.map[i].occupied {
                // Skip entries pointing to the excluded slot.
                if let Some((slot, generation)) = exclude_slot {
                    if self.map[i].slot == slot && self.map[i].generation == generation {
                        continue;
                    }
                }
                let p = self.map[i].priority;
                if p < requester_priority {
                    match best {
                        None => best = Some((i, p)),
                        Some((_, best_p)) if p < best_p => best = Some((i, p)),
                        _ => {}
                    }
                }
            }
        }
        best.map(|(idx, _)| idx)
    }

    fn lookup(&self, handle: RawHandle) -> Option<usize> {
        let entry = self.map.iter().find(|e| e.occupied && e.key == handle.0)?;
        let slot = &self.slots[entry.slot as usize];
        if slot.generation != entry.generation || slot.value.is_none() {
            return None;
        }
        Some(entry.slot as usize)
    }

    fn lookup_owned(&self, handle: RawHandle, owner: u16) -> Option<usize> {
        let entry = self.map.iter().find(|e| e.occupied && e.key == handle.0 && e.owner == owner)?;
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

    /// Get a mutable reference, but only if `owner` owns the handle.
    pub fn get_mut_owned(&mut self, handle: RawHandle, owner: u16) -> Option<&mut T> {
        let idx = self.lookup_owned(handle, owner)?;
        self.slots[idx].value.as_mut()
    }

    /// Release a handle entry and decrement the slot's refcount.
    /// If this was the last reference, the value is dropped and the slot's
    /// generation is incremented. Returns `Some(value)` only on last release.
    fn release_entry(&mut self, entry_idx: usize) -> Option<T> {
        let slot_idx = self.map[entry_idx].slot as usize;
        let entry_gen = self.map[entry_idx].generation;
        self.map[entry_idx].occupied = false;

        let slot = &mut self.slots[slot_idx];
        if slot.generation != entry_gen || slot.value.is_none() {
            return None;
        }
        slot.refcount = slot.refcount.saturating_sub(1);
        if slot.refcount == 0 {
            let value = slot.value.take();
            slot.generation = slot.generation.wrapping_add(1);
            value
        } else {
            None
        }
    }

    /// Remove a handle entry. If this was the last reference (refcount hits 0),
    /// the value is dropped and returned. Otherwise returns `None` (value still alive).
    pub fn remove(&mut self, handle: RawHandle) -> Option<T> {
        let entry_idx = self
            .map
            .iter()
            .position(|e| e.occupied && e.key == handle.0)?;
        self.release_entry(entry_idx)
    }

    /// Remove a handle entry, but only if `owner` owns it.
    pub fn remove_owned(&mut self, handle: RawHandle, owner: u16) -> Option<T> {
        let entry_idx = self
            .map
            .iter()
            .position(|e| e.occupied && e.key == handle.0 && e.owner == owner)?;
        self.release_entry(entry_idx)
    }

    /// Remove all resources owned by the given task index.
    /// Drops each removed value only when its refcount hits zero.
    pub fn remove_by_owner(&mut self, task_index: u16) {
        for i in 0..N {
            if self.map[i].occupied && self.map[i].owner == task_index {
                self.release_entry(i);
            }
        }
    }

    /// Transfer ownership of a handle to a new task.
    /// Returns `false` if the handle doesn't exist or isn't owned by `current_owner`.
    pub fn transfer(
        &mut self,
        handle: RawHandle,
        current_owner: u16,
        new_owner: u16,
        new_priority: i8,
    ) -> bool {
        if let Some(entry) = self
            .map
            .iter_mut()
            .find(|e| e.occupied && e.key == handle.0 && e.owner == current_owner)
        {
            entry.owner = new_owner;
            entry.priority = new_priority;
            true
        } else {
            false
        }
    }

    /// Clone a handle for a new owner. Creates a new map entry pointing to the
    /// same slot and increments the slot's refcount.
    pub fn clone_handle(
        &mut self,
        handle: RawHandle,
        owner: u16,
        new_owner: u16,
        priority: i8,
    ) -> Result<RawHandle, CloneError> {
        // Find source entry — only if the caller owns it.
        let (slot_idx, generation) = {
            let src = self
                .map
                .iter()
                .find(|e| e.occupied && e.key == handle.0 && e.owner == owner)
                .ok_or(CloneError::InvalidHandle)?;
            (src.slot, src.generation)
        };

        // Verify slot is valid.
        let slot = &self.slots[slot_idx as usize];
        if slot.generation != generation || slot.value.is_none() {
            return Err(CloneError::InvalidHandle);
        }

        // Check for refcount overflow.
        if slot.refcount == u16::MAX {
            return Err(CloneError::ArenaFull);
        }

        // Find free map entry, evicting a lower-priority entry if needed.
        let map_idx = match self.map.iter().position(|e| !e.occupied) {
            Some(idx) => idx,
            None => {
                let victim = self
                    .find_eviction_victim(priority, Some((slot_idx, generation)))
                    .ok_or(CloneError::ArenaFull)?;
                self.release_entry(victim);
                self.map
                    .iter()
                    .position(|e| !e.occupied)
                    .expect("ipc: arena has no free map entry after eviction")
            }
        };

        // Increment refcount (overflow checked above).
        self.slots[slot_idx as usize].refcount += 1;

        let key = self.next_key();
        self.map[map_idx] = HandleEntry {
            key,
            slot: slot_idx,
            generation,
            occupied: true,
            owner: new_owner,
            priority,
        };

        Ok(RawHandle(key))
    }

}

/// Shared arena with interior mutability for single-threaded Hubris tasks.
///
/// Wraps `Arena<T, N>` in `UnsafeCell` so all methods take `&self`.
/// This allows multiple dispatchers to hold `&SharedArena` references
/// simultaneously without borrow conflicts.
///
/// # Safety
///
/// Sound only in single-threaded contexts (Hubris tasks). No concurrent
/// access may occur to the same `SharedArena`.
pub struct SharedArena<T, const N: usize> {
    inner: core::cell::UnsafeCell<Arena<T, N>>,
}

// SAFETY: Hubris tasks are single-threaded. No concurrent access.
unsafe impl<T, const N: usize> Sync for SharedArena<T, N> {}

impl<T, const N: usize> SharedArena<T, N> {
    pub const fn new(kind: u8) -> Self {
        Self {
            inner: core::cell::UnsafeCell::new(Arena::new(kind)),
        }
    }

    fn arena(&self) -> &mut Arena<T, N> {
        unsafe { &mut *self.inner.get() }
    }

    pub fn get(&self, handle: RawHandle) -> Option<&T> {
        self.arena().get(handle)
    }

    pub fn get_mut(&self, handle: RawHandle) -> Option<&mut T> {
        self.arena().get_mut(handle)
    }

    pub fn get_mut_owned(&self, handle: RawHandle, owner: u16) -> Option<&mut T> {
        self.arena().get_mut_owned(handle, owner)
    }

    pub fn alloc(&self, value: T, owner: u16, priority: i8) -> Option<RawHandle> {
        self.arena().alloc(value, owner, priority)
    }

    pub fn remove(&self, handle: RawHandle) -> Option<T> {
        self.arena().remove(handle)
    }

    pub fn remove_owned(&self, handle: RawHandle, owner: u16) -> Option<T> {
        self.arena().remove_owned(handle, owner)
    }

    pub fn remove_by_owner(&self, task_index: u16) {
        self.arena().remove_by_owner(task_index);
    }

    pub fn transfer(
        &self,
        handle: RawHandle,
        current_owner: u16,
        new_owner: u16,
        new_priority: i8,
    ) -> bool {
        self.arena().transfer(handle, current_owner, new_owner, new_priority)
    }

    pub fn clone_handle(
        &self,
        handle: RawHandle,
        owner: u16,
        new_owner: u16,
        priority: i8,
    ) -> Result<RawHandle, CloneError> {
        self.arena().clone_handle(handle, owner, new_owner, priority)
    }
}
