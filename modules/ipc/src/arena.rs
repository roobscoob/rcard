use core::cell::Cell;

use crate::RawHandle;

/// Error returned by `Arena::clone_handle` and `Cloneable::clone_for`.
#[derive(Debug)]
pub enum CloneError {
    /// The source handle doesn't exist or isn't owned by the caller.
    InvalidHandle,
    /// The map has no free entries or the refcount would overflow.
    ArenaFull,
    /// The server hosting the handle died (IPC failed).
    ServerDied,
}

struct HandleEntry {
    key: Cell<u64>,
    slot: Cell<u8>,
    generation: Cell<u32>,
    occupied: Cell<bool>,
    owner: Cell<u16>,
    priority: Cell<i8>,
    /// If `Some(target)`, this handle is frozen mid-transfer (2PC pending state).
    /// `get`/`get_mut` reject pending entries. Eviction still applies normally.
    pending_to: Cell<Option<u16>>,
}

struct Slot<T> {
    value: core::cell::UnsafeCell<Option<T>>,
    generation: Cell<u32>,
    refcount: Cell<u16>,
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
    next_key: Cell<u64>,
}

impl<T, const N: usize> Arena<T, N> {
    const EMPTY_SLOT: Slot<T> = Slot {
        value: core::cell::UnsafeCell::new(None),
        generation: Cell::new(0),
        refcount: Cell::new(0),
    };

    const EMPTY_ENTRY: HandleEntry = HandleEntry {
        key: Cell::new(0),
        slot: Cell::new(0),
        generation: Cell::new(0),
        occupied: Cell::new(false),
        owner: Cell::new(0),
        priority: Cell::new(0),
        pending_to: Cell::new(None),
    };

    pub const fn new(kind: u8) -> Self {
        assert!(N <= 255, "Arena: N must be <= 255 (slot index is stored as u8)");
        Self {
            slots: [Self::EMPTY_SLOT; N],
            map: [Self::EMPTY_ENTRY; N],
            // Seed with kind byte so different arenas produce different key sequences.
            next_key: Cell::new((kind as u64) << 56 | 1),
        }
    }

    fn next_key(&self) -> u64 {
        let key = self.next_key.get();
        self.next_key.set(
            key.wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407),
        );
        key
    }

    /// Allocate a new slot, returning a handle to it.
    ///
    /// If the arena is full and `priority` is strictly greater than the
    /// lowest-priority occupied entry, that entry is evicted (its value is
    /// dropped and its generation is bumped) to make room.
    pub fn alloc(&self, value: T, owner: u16, priority: i8) -> Option<RawHandle> {
        // Find a free slot, or evict an entire slot to free one.
        let slot_idx = match self.slots.iter().position(|s| {
            // SAFETY: we only read the Option discriminant here, no outstanding &mut
            unsafe { (*s.value.get()).is_none() }
        }) {
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
            .position(|e| !e.occupied.get())
            .expect("ipc: arena has a free slot but no free map entry");

        let generation = self.slots[slot_idx].generation.get();
        // SAFETY: single-threaded, no other references to this slot's value
        unsafe { *self.slots[slot_idx].value.get() = Some(value); }
        self.slots[slot_idx].refcount.set(1);

        let key = self.next_key();

        self.map[map_idx].key.set(key);
        self.map[map_idx].slot.set(slot_idx as u8);
        self.map[map_idx].generation.set(generation);
        self.map[map_idx].occupied.set(true);
        self.map[map_idx].owner.set(owner);
        self.map[map_idx].priority.set(priority);
        self.map[map_idx].pending_to.set(None);

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
    fn evict_slot(&self, requester_priority: i8) -> Option<usize> {
        // Pass 1: for each occupied slot, compute max priority across its
        // map entries. Only consider slots where max_priority < requester.
        let mut best: Option<(u8, i8, u16)> = None; // (slot_idx, max_priority, refcount)
        for i in 0..N {
            // SAFETY: single-threaded, only reading discriminant
            if unsafe { (*self.slots[i].value.get()).is_none() } {
                continue;
            }
            let slot_idx = i as u8;
            let mut max_prio = i8::MIN;
            for j in 0..N {
                if self.map[j].occupied.get() && self.map[j].slot.get() == slot_idx
                    && self.map[j].generation.get() == self.slots[i].generation.get()
                {
                    if self.map[j].priority.get() > max_prio {
                        max_prio = self.map[j].priority.get();
                    }
                }
            }
            if max_prio >= requester_priority {
                continue;
            }
            let refcount = self.slots[i].refcount.get();
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
            if self.map[i].occupied.get() && self.map[i].slot.get() == victim_slot
                && self.map[i].generation.get() == self.slots[victim_slot as usize].generation.get()
            {
                self.release_entry(i);
            }
        }

        // The slot must now be free.
        debug_assert!(unsafe { (*self.slots[victim_slot as usize].value.get()).is_none() });
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
            if self.map[i].occupied.get() {
                // Skip entries pointing to the excluded slot.
                if let Some((slot, generation)) = exclude_slot {
                    if self.map[i].slot.get() == slot && self.map[i].generation.get() == generation {
                        continue;
                    }
                }
                let p = self.map[i].priority.get();
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
        let entry = self.map.iter().find(|e| e.occupied.get() && e.key.get() == handle.0 && e.pending_to.get().is_none())?;
        let slot = &self.slots[entry.slot.get() as usize];
        // SAFETY: single-threaded, only reading discriminant
        if slot.generation.get() != entry.generation.get() || unsafe { (*slot.value.get()).is_none() } {
            return None;
        }
        Some(entry.slot.get() as usize)
    }

    fn lookup_owned(&self, handle: RawHandle, owner: u16) -> Option<usize> {
        let entry = self.map.iter().find(|e| e.occupied.get() && e.key.get() == handle.0 && e.owner.get() == owner && e.pending_to.get().is_none())?;
        let slot = &self.slots[entry.slot.get() as usize];
        // SAFETY: single-threaded, only reading discriminant
        if slot.generation.get() != entry.generation.get() || unsafe { (*slot.value.get()).is_none() } {
            return None;
        }
        Some(entry.slot.get() as usize)
    }

    pub fn get(&self, handle: RawHandle) -> Option<&T> {
        let idx = self.lookup(handle)?;
        // SAFETY: single-threaded; lookup verified the slot is occupied
        unsafe { (*self.slots[idx].value.get()).as_ref() }
    }

    pub fn get_mut(&self, handle: RawHandle) -> Option<&mut T> {
        let idx = self.lookup(handle)?;
        // SAFETY: single-threaded; no two &mut T to the same slot simultaneously
        // in Hubris single-threaded tasks
        unsafe { (*self.slots[idx].value.get()).as_mut() }
    }

    /// Get a mutable reference, but only if `owner` owns the handle.
    pub fn get_mut_owned(&self, handle: RawHandle, owner: u16) -> Option<&mut T> {
        let idx = self.lookup_owned(handle, owner)?;
        // SAFETY: single-threaded; no two &mut T to the same slot simultaneously
        unsafe { (*self.slots[idx].value.get()).as_mut() }
    }

    /// Release a handle entry and decrement the slot's refcount.
    /// If this was the last reference, the value is dropped and the slot's
    /// generation is incremented. Returns `Some(value)` only on last release.
    fn release_entry(&self, entry_idx: usize) -> Option<T> {
        let slot_idx = self.map[entry_idx].slot.get() as usize;
        let entry_gen = self.map[entry_idx].generation.get();
        self.map[entry_idx].occupied.set(false);
        self.map[entry_idx].pending_to.set(None);

        let slot = &self.slots[slot_idx];
        // SAFETY: single-threaded, only reading discriminant
        if slot.generation.get() != entry_gen || unsafe { (*slot.value.get()).is_none() } {
            return None;
        }
        slot.refcount.set(slot.refcount.get().saturating_sub(1));
        if slot.refcount.get() == 0 {
            // SAFETY: single-threaded, refcount is 0 so no other references
            let value = unsafe { (*slot.value.get()).take() };
            slot.generation.set(slot.generation.get().wrapping_add(1));
            value
        } else {
            None
        }
    }

    /// Remove a handle entry. If this was the last reference (refcount hits 0),
    /// the value is dropped and returned. Otherwise returns `None` (value still alive).
    pub fn remove(&self, handle: RawHandle) -> Option<T> {
        let entry_idx = self
            .map
            .iter()
            .position(|e| e.occupied.get() && e.key.get() == handle.0)?;
        self.release_entry(entry_idx)
    }

    /// Remove a handle entry, but only if `owner` owns it.
    pub fn remove_owned(&self, handle: RawHandle, owner: u16) -> Option<T> {
        let entry_idx = self
            .map
            .iter()
            .position(|e| e.occupied.get() && e.key.get() == handle.0 && e.owner.get() == owner)?;
        self.release_entry(entry_idx)
    }

    /// Remove all resources owned by the given task index.
    /// Drops each removed value only when its refcount hits zero.
    pub fn remove_by_owner(&self, task_index: u16) {
        for i in 0..N {
            if self.map[i].occupied.get() && self.map[i].owner.get() == task_index {
                self.release_entry(i);
            }
        }
    }

    /// Clone a handle for a new owner. Creates a new map entry pointing to the
    /// same slot and increments the slot's refcount.
    pub fn clone_handle(
        &self,
        handle: RawHandle,
        owner: u16,
        new_owner: u16,
        priority: i8,
    ) -> Result<RawHandle, CloneError> {
        // Find source entry — only if the caller owns it and not mid-2PC.
        let (slot_idx, generation) = {
            let src = self
                .map
                .iter()
                .find(|e| e.occupied.get() && e.key.get() == handle.0 && e.owner.get() == owner && e.pending_to.get().is_none())
                .ok_or(CloneError::InvalidHandle)?;
            (src.slot.get(), src.generation.get())
        };

        // Verify slot is valid.
        let slot = &self.slots[slot_idx as usize];
        // SAFETY: single-threaded, only reading discriminant
        if slot.generation.get() != generation || unsafe { (*slot.value.get()).is_none() } {
            return Err(CloneError::InvalidHandle);
        }

        // Check for refcount overflow.
        if slot.refcount.get() == u16::MAX {
            return Err(CloneError::ArenaFull);
        }

        // Find free map entry, evicting a lower-priority entry if needed.
        let map_idx = match self.map.iter().position(|e| !e.occupied.get()) {
            Some(idx) => idx,
            None => {
                let victim = self
                    .find_eviction_victim(priority, Some((slot_idx, generation)))
                    .ok_or(CloneError::ArenaFull)?;
                self.release_entry(victim);
                self.map
                    .iter()
                    .position(|e| !e.occupied.get())
                    .expect("ipc: arena has no free map entry after eviction")
            }
        };

        // Increment refcount (overflow checked above).
        self.slots[slot_idx as usize].refcount.set(
            self.slots[slot_idx as usize].refcount.get() + 1,
        );

        let key = self.next_key();
        self.map[map_idx].key.set(key);
        self.map[map_idx].slot.set(slot_idx);
        self.map[map_idx].generation.set(generation);
        self.map[map_idx].occupied.set(true);
        self.map[map_idx].owner.set(new_owner);
        self.map[map_idx].priority.set(priority);
        self.map[map_idx].pending_to.set(None);

        Ok(RawHandle(key))
    }

    // -----------------------------------------------------------------------
    // Two-phase commit (2PC) transfer primitives
    // -----------------------------------------------------------------------

    /// Phase 1: freeze a handle for transfer to `target`.
    /// Returns `false` if the handle doesn't exist, isn't owned by `owner`,
    /// or is already pending transfer.
    pub fn prepare_transfer(&self, handle: RawHandle, owner: u16, target: u16) -> bool {
        if let Some(entry) = self.map.iter().find(|e| {
            e.occupied.get() && e.key.get() == handle.0 && e.owner.get() == owner && e.pending_to.get().is_none()
        }) {
            let slot = &self.slots[entry.slot.get() as usize];
            // SAFETY: single-threaded, only reading discriminant
            if slot.generation.get() != entry.generation.get() || unsafe { (*slot.value.get()).is_none() } {
                return false;
            }
            entry.pending_to.set(Some(target));
            true
        } else {
            false
        }
    }

    /// Cancel a pending transfer, unfreezing the handle.
    /// Returns `false` if the handle isn't found or isn't pending.
    pub fn cancel_transfer(&self, handle: RawHandle, owner: u16) -> bool {
        if let Some(entry) = self.map.iter().find(|e| {
            e.occupied.get() && e.key.get() == handle.0 && e.owner.get() == owner && e.pending_to.get().is_some()
        }) {
            entry.pending_to.set(None);
            true
        } else {
            false
        }
    }

    /// Phase 2: complete a transfer — the acquirer takes ownership.
    /// Only succeeds if `pending_to == Some(acquirer)`.
    pub fn acquire(&self, handle: RawHandle, acquirer: u16, new_priority: i8) -> bool {
        if let Some(entry) = self.map.iter().find(|e| {
            e.occupied.get() && e.key.get() == handle.0 && e.pending_to.get() == Some(acquirer)
        }) {
            let slot = &self.slots[entry.slot.get() as usize];
            // SAFETY: single-threaded, only reading discriminant
            if slot.generation.get() != entry.generation.get() || unsafe { (*slot.value.get()).is_none() } {
                return false;
            }
            entry.owner.set(acquirer);
            entry.priority.set(new_priority);
            entry.pending_to.set(None);
            true
        } else {
            false
        }
    }

    /// Drop a handle if it exists and is owned by `owner`, even if pending.
    /// Used for cleanup after a failed 2PC transfer. Does NOT use `lookup_owned`
    /// (which rejects pending entries).
    pub fn try_drop(&self, handle: RawHandle, owner: u16) -> bool {
        if let Some(idx) = self.map.iter().position(|e| {
            e.occupied.get() && e.key.get() == handle.0 && e.owner.get() == owner
        }) {
            self.release_entry(idx);
            true
        } else {
            false
        }
    }

    /// Cancel all pending transfers targeting `target`.
    /// Called when a target task dies — unfreezes any handles that were
    /// prepared for transfer to it.
    pub fn cancel_transfers_to(&self, target: u16) {
        for i in 0..N {
            if self.map[i].occupied.get() && self.map[i].pending_to.get() == Some(target) {
                let slot = &self.slots[self.map[i].slot.get() as usize];
                // SAFETY: single-threaded, only reading discriminant
                if slot.generation.get() == self.map[i].generation.get()
                    && unsafe { !(*slot.value.get()).is_none() }
                {
                    self.map[i].pending_to.set(None);
                }
            }
        }
    }

}

/// Shared arena with interior mutability for single-threaded Hubris tasks.
///
/// Wraps `Arena<T, N>` so all methods take `&self`.
/// The underlying `Arena` uses `Cell` for metadata and `UnsafeCell` for values,
/// so `SharedArena` only needs a shared reference to the inner arena.
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

    fn arena(&self) -> &Arena<T, N> {
        // SAFETY: single-threaded Hubris tasks. Arena methods use Cell/UnsafeCell
        // internally, so a shared reference is sufficient.
        unsafe { &*self.inner.get() }
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

    pub fn clone_handle(
        &self,
        handle: RawHandle,
        owner: u16,
        new_owner: u16,
        priority: i8,
    ) -> Result<RawHandle, CloneError> {
        self.arena().clone_handle(handle, owner, new_owner, priority)
    }

    pub fn prepare_transfer(&self, handle: RawHandle, owner: u16, target: u16) -> bool {
        self.arena().prepare_transfer(handle, owner, target)
    }

    pub fn cancel_transfer(&self, handle: RawHandle, owner: u16) -> bool {
        self.arena().cancel_transfer(handle, owner)
    }

    pub fn acquire(&self, handle: RawHandle, acquirer: u16, new_priority: i8) -> bool {
        self.arena().acquire(handle, acquirer, new_priority)
    }

    pub fn try_drop(&self, handle: RawHandle, owner: u16) -> bool {
        self.arena().try_drop(handle, owner)
    }

    pub fn cancel_transfers_to(&self, target: u16) {
        self.arena().cancel_transfers_to(target);
    }
}
