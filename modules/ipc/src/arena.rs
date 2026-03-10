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
    parent: Option<RawHandle>,
    /// Priority of the owner at the time of allocation.
    /// Used for eviction: a higher-priority requester can evict a lower-priority holder.
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
///
/// Supports priority-based eviction: when the arena is full, a requester
/// with strictly higher priority than the lowest-priority current holder
/// can evict that holder's handle. Evicted keys are recorded in a tombstone
/// ring so clients can distinguish eviction (`HandleLost`) from programming
/// errors (`INVALID_HANDLE`).
pub struct Arena<T, const N: usize> {
    slots: [Slot<T>; N],
    map: [HandleEntry; N],
    /// Ring buffer of recently-evicted handle keys.
    /// When lookup_owned fails, we check here to return HandleLost vs panic.
    tombstone: [u64; N],
    tombstone_len: usize,
    tombstone_head: usize,
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
        parent: None,
        priority: 0,
    };

    pub const fn new(kind: u8) -> Self {
        assert!(N <= 255, "Arena: N must be <= 255 (slot index is stored as u8)");
        Self {
            slots: [Self::EMPTY_SLOT; N],
            map: [Self::EMPTY_ENTRY; N],
            tombstone: [0u64; N],
            tombstone_len: 0,
            tombstone_head: 0,
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
    /// `priority` is the requester's declared priority for this sysmodule.
    /// When the arena is full, a requester with strictly higher priority than
    /// the lowest-priority current holder will evict that holder's handle.
    /// Equal priority → returns `None` (ArenaFull). Evicted keys are recorded
    /// in the tombstone ring so clients get `HandleLost` rather than a panic.
    pub fn alloc(&mut self, value: T, owner: u16, priority: i8) -> Option<RawHandle> {
        self.alloc_inner(value, owner, priority, None)
    }

    /// Allocate a new slot with a parent handle reference.
    /// When the parent is destroyed, `remove_by_parent` can cascade-delete children.
    pub fn alloc_with_parent(
        &mut self,
        value: T,
        owner: u16,
        priority: i8,
        parent: RawHandle,
    ) -> Option<RawHandle> {
        self.alloc_inner(value, owner, priority, Some(parent))
    }

    fn alloc_inner(
        &mut self,
        value: T,
        owner: u16,
        priority: i8,
        parent: Option<RawHandle>,
    ) -> Option<RawHandle> {
        // Fast path: free slot available.
        let slot_idx_opt = self.slots.iter().position(|s| s.value.is_none());
        let map_idx_opt = self.map.iter().position(|e| !e.occupied);

        let (slot_idx, map_idx) = match (slot_idx_opt, map_idx_opt) {
            (Some(s), Some(m)) => (s, m),
            _ => {
                // Arena full — attempt eviction.
                // Find the map entry with the lowest priority that is strictly
                // less than `priority`. If multiple entries tie for lowest,
                // pick the first one (deterministic).
                let victim_idx = self
                    .map
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.occupied)
                    .min_by_key(|(_, e)| e.priority)
                    .and_then(|(i, e)| {
                        if e.priority < priority {
                            Some(i)
                        } else {
                            None
                        }
                    });

                let victim_idx = victim_idx?; // None → ArenaFull, requester loses

                // Record the evicted key in the tombstone ring before releasing.
                let evicted_key = self.map[victim_idx].key;
                self.tombstone_push(evicted_key);

                // Release the victim entry (may free the slot if refcount hits 0).
                let victim_slot = self.map[victim_idx].slot as usize;
                let victim_handle = RawHandle(evicted_key);
                self.release_entry(victim_idx);
                // Cascade-remove any children of the evicted handle.
                self.remove_by_parent_tombstoning(victim_handle);

                // The slot may now be free. If not (still has other refs via
                // clone), we can't reuse it — fall back to ArenaFull.
                if self.slots[victim_slot].value.is_some() {
                    return None;
                }

                // Find the now-free map entry.
                let map_idx = self.map.iter().position(|e| !e.occupied)?;
                (victim_slot, map_idx)
            }
        };

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
            parent,
            priority,
        };

        Some(RawHandle(key))
    }

    /// Push a key into the tombstone ring, overwriting the oldest entry when full.
    fn tombstone_push(&mut self, key: u64) {
        self.tombstone[self.tombstone_head] = key;
        self.tombstone_head = (self.tombstone_head + 1) % N;
        if self.tombstone_len < N {
            self.tombstone_len += 1;
        }
    }

    /// Check whether a key is in the tombstone ring (was recently evicted).
    pub fn is_tombstoned(&self, key: u64) -> bool {
        self.tombstone[..self.tombstone_len].contains(&key)
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

    /// Check whether a handle was evicted (vs never existing / wrong owner).
    /// Returns `true` if the key is in the tombstone ring.
    /// Used by server dispatch to reply `HANDLE_EVICTED` vs `INVALID_HANDLE`.
    pub fn was_evicted(&self, handle: RawHandle) -> bool {
        self.is_tombstoned(handle.0)
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
    /// Cascades: also removes children of each removed handle.
    pub fn remove_by_owner(&mut self, task_index: u16) {
        for i in 0..N {
            if self.map[i].occupied && self.map[i].owner == task_index {
                let handle = RawHandle(self.map[i].key);
                self.release_entry(i);
                self.remove_by_parent(handle);
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
    ) -> bool {
        if let Some(entry) = self
            .map
            .iter_mut()
            .find(|e| e.occupied && e.key == handle.0 && e.owner == current_owner)
        {
            entry.owner = new_owner;
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
        new_priority: i8,
    ) -> Result<RawHandle, CloneError> {
        // Find source entry — only if the caller owns it.
        let (slot_idx, generation, parent) = {
            let src = self
                .map
                .iter()
                .find(|e| e.occupied && e.key == handle.0 && e.owner == owner)
                .ok_or(CloneError::InvalidHandle)?;
            (src.slot, src.generation, src.parent)
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

        // Find free map entry.
        let map_idx = self
            .map
            .iter()
            .position(|e| !e.occupied)
            .ok_or(CloneError::ArenaFull)?;

        // Increment refcount (overflow checked above).
        self.slots[slot_idx as usize].refcount += 1;

        let key = self.next_key();
        self.map[map_idx] = HandleEntry {
            key,
            slot: slot_idx,
            generation,
            occupied: true,
            owner: new_owner,
            parent,
            priority: new_priority,
        };

        Ok(RawHandle(key))
    }

    /// Remove all entries whose parent matches the given handle, recording
    /// evicted keys in the tombstone ring. Used during eviction cascades.
    fn remove_by_parent_tombstoning(&mut self, parent: RawHandle) {
        let mut pending = [RawHandle(0); N];
        let mut pending_len = 1;
        pending[0] = parent;

        while pending_len > 0 {
            pending_len -= 1;
            let current_parent = pending[pending_len];

            for i in 0..N {
                if self.map[i].occupied {
                    if let Some(p) = self.map[i].parent {
                        if p == current_parent {
                            let child_key = self.map[i].key;
                            self.tombstone_push(child_key);
                            let child = RawHandle(child_key);
                            self.release_entry(i);
                            if pending_len < N {
                                pending[pending_len] = child;
                                pending_len += 1;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Remove all entries whose parent matches the given handle.
    /// Cascades: children of removed entries are also removed.
    /// Values are dropped when their refcount hits zero.
    ///
    /// Uses an iterative approach to avoid stack overflow on deep hierarchies.
    pub fn remove_by_parent(&mut self, parent: RawHandle) {
        // Pending parents whose children need removal. N entries is the
        // theoretical max since each entry can be a parent at most once.
        let mut pending = [RawHandle(0); N];
        let mut pending_len = 1;
        pending[0] = parent;

        while pending_len > 0 {
            pending_len -= 1;
            let current_parent = pending[pending_len];

            for i in 0..N {
                if self.map[i].occupied {
                    if let Some(p) = self.map[i].parent {
                        if p == current_parent {
                            let child = RawHandle(self.map[i].key);
                            self.release_entry(i);
                            // Queue this child as a parent for cascade.
                            if pending_len < N {
                                pending[pending_len] = child;
                                pending_len += 1;
                            }
                        }
                    }
                }
            }
        }
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

    pub fn alloc_with_parent(
        &self,
        value: T,
        owner: u16,
        priority: i8,
        parent: RawHandle,
    ) -> Option<RawHandle> {
        self.arena().alloc_with_parent(value, owner, priority, parent)
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

    pub fn remove_by_parent(&self, parent: RawHandle) {
        self.arena().remove_by_parent(parent);
    }

    pub fn transfer(
        &self,
        handle: RawHandle,
        current_owner: u16,
        new_owner: u16,
    ) -> bool {
        self.arena().transfer(handle, current_owner, new_owner)
    }

    pub fn clone_handle(
        &self,
        handle: RawHandle,
        owner: u16,
        new_owner: u16,
        new_priority: i8,
    ) -> Result<RawHandle, CloneError> {
        self.arena().clone_handle(handle, owner, new_owner, new_priority)
    }

    pub fn was_evicted(&self, handle: RawHandle) -> bool {
        self.arena().was_evicted(handle)
    }
}
