//! Global allocator for the compositor sysmodule.
//!
//! Single-threaded first-fit linked-list freelist allocator with neighbor
//! coalescing on free. Backs the compositor's PSRAM `frame_buffers` region
//! and serves lilla-oxid's `Image::data: Vec<u8>`.
//!
//! Block layout:
//!   allocated:  [size: usize][user data ...]
//!   free:       [size: usize][next: *mut FreeNode][...]
//!
//! All blocks are 8-byte aligned; the size header is 8-byte aligned and
//! includes itself in the recorded size. The free list is sorted by address
//! so coalescing on `dealloc` only walks until it finds the predecessor,
//! and the next-neighbor coalesce check is O(1).

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::mem;
use core::ptr;

const ALIGN: usize = 8;
const HEADER_SIZE: usize = mem::size_of::<usize>();

#[repr(C, align(8))]
struct FreeNode {
    /// Total block size in bytes, including the header. Always a multiple of `ALIGN`
    /// and at least `mem::size_of::<FreeNode>()`.
    size: usize,
    /// Next free block in the address-sorted freelist, or null at the tail.
    next: *mut FreeNode,
}

struct Inner {
    head: *mut FreeNode,
}

pub struct Heap {
    inner: UnsafeCell<Inner>,
}

// SAFETY: this allocator is intended for single-threaded Hubris tasks. The
// `Sync` impl exists only so the `Heap` can be placed in a `static`.
unsafe impl Sync for Heap {}

impl Heap {
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(Inner {
                head: ptr::null_mut(),
            }),
        }
    }

    /// Initialize the heap from a memory region of `len` bytes starting at `base`.
    ///
    /// # Safety
    ///
    /// `base..base+len` must be valid for reads and writes for `'static`,
    /// must not overlap any other live allocation, and must not be referenced
    /// elsewhere. Calling this more than once is undefined behavior.
    pub unsafe fn init(&self, base: *mut u8, len: usize) {
        let offset = base.align_offset(ALIGN);
        if offset >= len {
            return;
        }
        let aligned = unsafe { base.add(offset) };
        let usable = (len - offset) & !(ALIGN - 1);
        if usable < mem::size_of::<FreeNode>() {
            return;
        }
        let node = aligned as *mut FreeNode;
        unsafe {
            (*node).size = usable;
            (*node).next = ptr::null_mut();
            (*self.inner.get()).head = node;
        }
    }
}

/// Round `(HEADER_SIZE + payload)` up to the allocator's block granularity.
#[inline]
fn block_size_for(payload: usize) -> usize {
    let raw = HEADER_SIZE.saturating_add(payload.max(1));
    let rounded = (raw + ALIGN - 1) & !(ALIGN - 1);
    rounded.max(mem::size_of::<FreeNode>())
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Alignments above ALIGN are not supported; lilla-oxid only allocates
        // Vec<u8> (align 1), and we don't have a use case for higher.
        if layout.align() > ALIGN {
            return ptr::null_mut();
        }
        let total = block_size_for(layout.size());

        let inner = unsafe { &mut *self.inner.get() };
        let mut prev: *mut *mut FreeNode = &mut inner.head;
        loop {
            let cur = unsafe { *prev };
            if cur.is_null() {
                return ptr::null_mut();
            }
            let cur_size = unsafe { (*cur).size };
            if cur_size >= total {
                let next = unsafe { (*cur).next };
                let remainder = cur_size - total;
                if remainder >= mem::size_of::<FreeNode>() {
                    // Split: front becomes the allocation, tail becomes a new free block.
                    let leftover = unsafe { (cur as *mut u8).add(total) as *mut FreeNode };
                    unsafe {
                        (*leftover).size = remainder;
                        (*leftover).next = next;
                        *prev = leftover;
                        *(cur as *mut usize) = total;
                    }
                } else {
                    // Take the whole block.
                    unsafe {
                        *prev = next;
                        *(cur as *mut usize) = cur_size;
                    }
                }
                return unsafe { (cur as *mut u8).add(HEADER_SIZE) };
            }
            prev = unsafe { &mut (*cur).next };
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        let block = unsafe { ptr.sub(HEADER_SIZE) } as *mut FreeNode;
        let size = unsafe { *(block as *const usize) };

        let inner = unsafe { &mut *self.inner.get() };

        // Insert into the address-sorted freelist.
        let mut prev: *mut *mut FreeNode = &mut inner.head;
        loop {
            let cur = unsafe { *prev };
            if cur.is_null() || (cur as *const u8) > (block as *const u8) {
                break;
            }
            prev = unsafe { &mut (*cur).next };
        }
        unsafe {
            (*block).size = size;
            (*block).next = *prev;
            *prev = block;
        }

        // Coalesce with the next free block if it abuts.
        unsafe {
            let next = (*block).next;
            if !next.is_null() && (block as *mut u8).add((*block).size) == next as *mut u8 {
                (*block).size += (*next).size;
                (*block).next = (*next).next;
            }
        }

        // Coalesce with the previous free block if it abuts. The freelist is
        // address-sorted so a linear scan from head finds the predecessor.
        let mut prev_node: *mut FreeNode = ptr::null_mut();
        let mut cur = inner.head;
        while !cur.is_null() && cur != block {
            prev_node = cur;
            cur = unsafe { (*cur).next };
        }
        if !prev_node.is_null() {
            unsafe {
                let prev_size = (*prev_node).size;
                if (prev_node as *mut u8).add(prev_size) == block as *mut u8 {
                    (*prev_node).size = prev_size + (*block).size;
                    (*prev_node).next = (*block).next;
                }
            }
        }
    }
}
