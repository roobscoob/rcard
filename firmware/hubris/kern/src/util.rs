// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Common utility functions used in various places in the kernel.

use kerncore::MemoryRegion;

/// Utility routine for getting `&mut` to _two_ elements of a slice, at indexes
/// `i` and `j`. `i` and `j` must be distinct, or this will panic.
#[inline(always)]
pub fn index2_distinct<T>(
    elements: &mut [T],
    i: usize,
    j: usize,
) -> (&mut T, &mut T) {
    if i < elements.len() && j < elements.len() && i != j {
        let base = elements.as_mut_ptr();
        // Safety:
        // - i is a valid offset for elements (checked above), base.add(i) is ok
        // - j is a valid offset for elements (checked above), base.add(j) is ok
        // - i and j do not alias (checked above), so we can dereference both
        // - The &muts are returned with the same lifetime as elements,
        //   preventing the caller from producing further aliasing.
        unsafe {
            let iptr = base.add(i);
            let jptr = base.add(j);
            (&mut *iptr, &mut *jptr)
        }
    } else {
        panic!()
    }
}

pub fn regions_overlap(
    offset: u32,
    length: u32,
    region: impl MemoryRegion,
) -> bool {
    let input_end = offset.saturating_add(length);

    if offset >= input_end {
        return false; // empty
    }

    let region_base = region.base_addr() as u32;
    let region_end = region.end_addr() as u32;

    if region_base == region_end {
        return false;
    }

    offset < region_end && region_base < input_end
}
