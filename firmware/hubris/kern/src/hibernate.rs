use core::cmp::Ordering;
use core::mem::MaybeUninit;
use core::ops::Deref;
use core::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

/// This module contains the implementation of task hibernation.
use abi::{AllocateError, HibernationEventId};
use kerncore::MemoryRegion;

use crate::atomic::AtomicExt;

pub const MAX_HIBERNATION_REGIONS: usize = 32;

// Slot ids are stored as u8 throughout.
const _: () = assert!(MAX_HIBERNATION_REGIONS <= u8::MAX as usize);

#[derive(Debug, Clone, Copy)]
pub struct HibernationDescriptor {
    pub generation: u32,
    pub slot_id: u8,
    pub region_pointer: u32,
    pub region_length: u32,
    // TODO: MAC + MAC key for hashing/validation. The MAC key should impl
    // Drop for zeroization; the table's `remove_at` helper drops removed
    // descriptors, so adding it requires no further changes here.
}

impl HibernationDescriptor {
    /// Exclusive end pointer. Non-overflowing: validated at insert time.
    fn end_pointer(&self) -> u32 {
        self.region_pointer + self.region_length
    }
}

impl MemoryRegion for HibernationDescriptor {
    fn base_addr(&self) -> usize {
        self.region_pointer as usize
    }
    fn end_addr(&self) -> usize {
        self.end_pointer() as usize
    }
    fn contains(&self, addr: usize) -> bool {
        (self.base_addr()..self.end_addr()).contains(&addr)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventStatus {
    Hibernating,
    Restored,
}

pub struct HibernationTable {
    /// Backing storage. `entries[0..count]` is initialized; the rest is
    /// uninit. Maintained sorted ascending by `region_pointer` and
    /// non-overlapping: for adjacent i, i+1,
    ///   entries[i].end_pointer() <= entries[i+1].region_pointer.
    entries: [MaybeUninit<HibernationDescriptor>; MAX_HIBERNATION_REGIONS],
    count: usize,

    /// Per-slot generation counter. `next_generation[id]` is the generation
    /// the next allocation of this slot id will receive. Strictly increases
    /// over the lifetime of the table; an event id with
    /// `generation() >= next_generation[id]` was never issued.
    next_generation: [u32; MAX_HIBERNATION_REGIONS],
}

impl HibernationTable {
    /// View of the initialized prefix as a slice.
    fn entries(&self) -> &[HibernationDescriptor] {
        // Safety: entries[0..count] are initialized per the type invariant,
        // and MaybeUninit<T> is layout-compatible with T.
        unsafe {
            core::slice::from_raw_parts(
                self.entries.as_ptr() as *const HibernationDescriptor,
                self.count,
            )
        }
    }

    /// Insert `descriptor` at index `at`, shifting `entries[at..count]`
    /// right by one. Drops nothing: every write target is uninit.
    ///
    /// Precondition: `at <= self.count < MAX_HIBERNATION_REGIONS`.
    fn insert_at(&mut self, at: usize, descriptor: HibernationDescriptor) {
        debug_assert!(at <= self.count);
        debug_assert!(self.count < MAX_HIBERNATION_REGIONS);

        // Reverse iteration: each step moves an init cell into the cell to
        // its right, which is uninit at that point (either initially uninit
        // beyond `count`, or vacated by the previous iteration's read).
        for i in (at..self.count).rev() {
            // Safety: i < count → entries[i] is init.
            //         entries[i + 1] is logically uninit at this point.
            let v = unsafe { self.entries[i].assume_init_read() };
            self.entries[i + 1].write(v);
        }
        // entries[at] is now logically uninit.
        self.entries[at].write(descriptor);
        self.count += 1;
    }

    /// Drop the entry at `at` and shift `entries[at+1..count]` left by one.
    ///
    /// Precondition: `at < self.count`.
    fn remove_at(&mut self, at: usize) {
        debug_assert!(at < self.count);

        // Safety: at < count → entries[at] is init. We drop it and treat
        // the cell as logically uninit for the shift below.
        unsafe { self.entries[at].assume_init_drop() };

        // Forward iteration: each step moves an init cell into the cell to
        // its left, which is uninit (either just-dropped at i = at + 1, or
        // vacated by the previous iteration's read).
        for i in (at + 1)..self.count {
            // Safety: i < count → entries[i] is init.
            //         entries[i - 1] is logically uninit at this point.
            let v = unsafe { self.entries[i].assume_init_read() };
            self.entries[i - 1].write(v);
        }
        self.count -= 1;
    }
}

impl HibernationTable {
    pub fn with_borrow<R>(body: impl FnOnce(&mut Self) -> R) -> R {
        static IN_USE: AtomicBool = AtomicBool::new(false);
        static mut TABLE: HibernationTable = HibernationTable {
            entries: [const { MaybeUninit::uninit() }; MAX_HIBERNATION_REGIONS],
            count: 0,
            next_generation: [0; MAX_HIBERNATION_REGIONS],
        };

        if IN_USE.swap_polyfill(true, AtomicOrdering::Acquire) {
            panic!("recursive use of HibernationTable");
        }

        // Safety: the swap returned false, so no other &mut to TABLE exists.
        let table = unsafe { &mut *core::ptr::addr_of_mut!(TABLE) };

        let v = body(table);

        IN_USE.store(false, AtomicOrdering::Release);

        v
    }

    /// Position in `entries` of the descriptor with this slot id, if any.
    /// Linear scan; bounded by MAX_HIBERNATION_REGIONS = 32.
    fn position_of(&self, slot_id: u8) -> Option<usize> {
        self.entries().iter().position(|d| d.slot_id == slot_id)
    }

    /// Resolve an event id to its descriptor, iff the slot is currently
    /// hibernating at the same generation.
    pub fn lookup(
        &self,
        event_id: HibernationEventId,
    ) -> Option<&HibernationDescriptor> {
        let descriptor = self
            .entries()
            .iter()
            .find(|d| d.slot_id == event_id.index())?;
        (descriptor.generation == event_id.generation()).then_some(descriptor)
    }

    /// Status of `event_id`, or `None` if it was never issued.
    pub fn status(&self, event_id: HibernationEventId) -> Option<EventStatus> {
        // Out-of-range index → never issued. `.get` makes the check total.
        let next_gen = *self.next_generation.get(event_id.index() as usize)?;
        if event_id.generation() >= next_gen {
            return None; // future generation
        }
        Some(match self.lookup(event_id) {
            Some(_) => EventStatus::Hibernating,
            None => EventStatus::Restored,
        })
    }

    /// Insert a new region, maintaining sort order and rejecting overlaps.
    pub fn allocate(
        &mut self,
        pointer: u32,
        length: u32,
    ) -> Result<(HibernationEventId, HibernationDescriptor), AllocateError>
    {
        // 1. Validate region.
        if length == 0 {
            return Err(AllocateError::InvalidRegion);
        }
        let region_end = pointer
            .checked_add(length)
            .ok_or(AllocateError::InvalidRegion)?;

        // 2. Capacity.
        if self.count >= MAX_HIBERNATION_REGIONS {
            return Err(AllocateError::Full);
        }

        // 3. Find sorted insertion point.
        let insert_at = self
            .entries()
            .partition_point(|d| d.region_pointer < pointer);

        // 4. Check overlap with the at-most-two neighbors of `insert_at`.
        //    Sort + non-overlap invariant means no other entry can overlap.
        if insert_at > 0
            && self.entries()[insert_at - 1].end_pointer() > pointer
        {
            return Err(AllocateError::Overlap);
        }
        if self
            .entries()
            .get(insert_at)
            .is_some_and(|next| next.region_pointer < region_end)
        {
            return Err(AllocateError::Overlap);
        }

        // 5. Pick a free slot id. With count < MAX, one exists.
        let slot_id = (0..MAX_HIBERNATION_REGIONS as u8)
            .find(|&id| self.position_of(id).is_none())
            .expect("count < MAX_HIBERNATION_REGIONS implies a free slot id");

        // 6. Issue a fresh generation; panic on u32 overflow.
        let generation = self.next_generation[slot_id as usize];
        self.next_generation[slot_id as usize] = generation
            .checked_add(1)
            .expect("hibernation slot generation overflow");

        // 7. Insert. The helper handles the shift; no back-refs to fix.
        self.insert_at(
            insert_at,
            HibernationDescriptor {
                generation,
                slot_id,
                region_pointer: pointer,
                region_length: length,
            },
        );

        Ok((
            HibernationEventId::from_parts(slot_id, generation),
            self.entries()[insert_at],
        ))
    }

    /// Remove the entry identified by `event_id`. After this returns,
    /// `status(event_id)` is `Some(Restored)` and `lookup(event_id)` is
    /// `None`. Returns `Err(())` if the event id is stale or out of range.
    pub fn restore(&mut self, event_id: HibernationEventId) -> Result<(), ()> {
        if (event_id.index() as usize) >= MAX_HIBERNATION_REGIONS {
            return Err(());
        }
        let pos = self.position_of(event_id.index()).ok_or(())?;
        if self.entries()[pos].generation != event_id.generation() {
            return Err(());
        }
        // Drops the descriptor and shifts. `next_generation` is preserved,
        // so the next allocation of this slot strictly increases generation.
        self.remove_at(pos);
        Ok(())
    }

    /// Returns true iff `region` overlaps any currently hibernating region.
    pub fn is_partially_hibernated<R: MemoryRegion>(&self, region: &R) -> bool {
        let base = region.base_addr();
        let end = region.end_addr();
        if base >= end {
            // Empty / degenerate region cannot overlap anything.
            return false;
        }

        // Same pattern as the access-check blacklist: binary search for
        // `base` against an interval comparison. If `base` lands inside an
        // entry, that entry overlaps. Otherwise the only entry that can
        // overlap is the one at the insertion point — its start, if it
        // exists, is the smallest start >= base, so if it's still less
        // than `end`, it overlaps.
        match self.entries().binary_search_by(|d| {
            if d.end_addr() <= base {
                Ordering::Less
            } else if d.base_addr() > base {
                Ordering::Greater
            } else {
                Ordering::Equal // base is inside d
            }
        }) {
            Ok(_) => true,
            Err(idx) => {
                self.entries().get(idx).is_some_and(|d| d.base_addr() < end)
            }
        }
    }

    pub fn is_fully_hibernated(&self, base: u32, length: u32) -> bool {
        let Some(end) = base.checked_add(length) else {
            return false;
        };

        if base >= end {
            return false;
        }

        let mut idx = match self.entries().binary_search_by(|d| {
            if d.end_addr() <= base as usize {
                Ordering::Less
            } else if d.base_addr() > base as usize {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }) {
            Ok(i) => i,
            Err(_) => return false, // base not covered → gap at the start
        };

        let mut covered_to = self.entries()[idx].end_addr();
        while covered_to < end as usize {
            idx += 1;
            match self.entries().get(idx) {
                Some(next) if next.base_addr() == covered_to => {
                    covered_to = next.end_addr();
                }
                _ => return false, // gap, or ran off the end
            }
        }
        true
    }
}

impl Deref for HibernationTable {
    type Target = [HibernationDescriptor];
    fn deref(&self) -> &Self::Target {
        self.entries()
    }
}

impl Drop for HibernationTable {
    fn drop(&mut self) {
        // The static instance never drops in practice, but this preserves
        // the invariant for any other use (tests, future stack instances).
        for i in 0..self.count {
            // Safety: i < count → entries[i] init, and we won't observe
            // these slots again after drop.
            unsafe { self.entries[i].assume_init_drop() };
        }
    }
}

#[repr(C)]
pub enum HibernateBuffer {
    Ok { data: [u8; 256], length: u16 },
    Reinitialize,
}

impl HibernateBuffer {
    pub fn reinitialize() -> Self {
        Self::Reinitialize
    }

    pub fn ok(data: &[u8]) -> Result<Self, ()> {
        if data.len() > 256 {
            return Err(());
        }

        Ok(Self {
            data: {
                let mut buffer = [0; 256];
                buffer[..data.len()].copy_from_slice(data);
                buffer
            },
            length: data.len() as u16,
        })
    }
}
