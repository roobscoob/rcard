use core::mem::MaybeUninit;

#[derive(Debug)]
pub enum AllocError {
    OutOfChunks,
    UnknownAllocation,
    Overflow,
}

pub struct Allocator<'a, const CHUNK_SIZE: usize, const CHUNK_COUNT: usize> {
    buffer: &'a mut [MaybeUninit<u8>],
    owners: [Option<(u32, usize)>; CHUNK_COUNT],
}

impl<'a, const CHUNK_SIZE: usize, const CHUNK_COUNT: usize> Allocator<'a, CHUNK_SIZE, CHUNK_COUNT> {
    pub fn new<const N: usize>(buf: &'a mut MaybeUninit<[u8; N]>) -> Self {
        const { assert!(N >= CHUNK_SIZE * CHUNK_COUNT) };
        // Safety: MaybeUninit<[u8; N]> and [MaybeUninit<u8>; N] have identical layout,
        // and MaybeUninit<u8> has no validity requirements.
        let buffer = unsafe { &mut *core::ptr::slice_from_raw_parts_mut(buf.as_mut_ptr().cast::<MaybeUninit<u8>>(), N) };
        Self {
            buffer,
            owners: [None; CHUNK_COUNT],
        }
    }

    /// # Safety
    ///
    /// The caller must ensure that `len` bytes starting at `idx * CHUNK_SIZE`
    /// have been initialized (i.e. were previously written via `write`).
    unsafe fn chunk_initialized(&self, idx: usize, len: usize) -> &[u8] {
        let start = idx * CHUNK_SIZE;
        unsafe { core::slice::from_raw_parts(self.buffer[start..].as_ptr().cast::<u8>(), len) }
    }

    fn chunk_mut(&mut self, idx: usize) -> &mut [MaybeUninit<u8>] {
        let start = idx * CHUNK_SIZE;
        &mut self.buffer[start..start + CHUNK_SIZE]
    }

    pub fn free(&mut self, allocation_id: u32) {
        for owner in self.owners.iter_mut() {
            if matches!(owner, Some((id, _)) if *id == allocation_id) {
                *owner = None;
            }
        }
    }

    /// Write `bytes` into the allocation identified by `allocation_id`.
    ///
    /// Any previous data for this allocation is freed first, then the bytes are
    /// spread across as many chunks as needed.
    pub fn write(&mut self, allocation_id: u32, bytes: &[u8]) -> Result<(), AllocError> {
        self.free(allocation_id);

        let chunks_needed = bytes.len().div_ceil(CHUNK_SIZE);
        let free_count = self.owners.iter().filter(|o| o.is_none()).count();
        if free_count < chunks_needed {
            return Err(AllocError::OutOfChunks);
        }

        let mut remaining = bytes;
        for idx in 0..CHUNK_COUNT {
            if remaining.is_empty() {
                break;
            }
            if self.owners[idx].is_some() {
                continue;
            }
            let take = remaining.len().min(CHUNK_SIZE);
            // Safety: writing initialized bytes into MaybeUninit storage via raw pointer copy.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    remaining.as_ptr(),
                    self.chunk_mut(idx).as_mut_ptr().cast::<u8>(),
                    take,
                );
            }
            self.owners[idx] = Some((allocation_id, take));
            remaining = &remaining[take..];
        }

        Ok(())
    }

    /// Reserve space for `len` bytes and return a writer that accepts data
    /// in arbitrarily-sized slices.
    ///
    /// Any previous data for this allocation is freed first, then enough
    /// chunks are claimed up front. Returns `OutOfChunks` if there isn't
    /// enough room.
    pub fn begin_write(&mut self, allocation_id: u32, len: usize) -> Result<ChunkWriter<'_, 'a, CHUNK_SIZE, CHUNK_COUNT>, AllocError> {
        self.free(allocation_id);

        let chunks_needed = len.div_ceil(CHUNK_SIZE);
        let free_count = self.owners.iter().filter(|o| o.is_none()).count();
        if free_count < chunks_needed {
            return Err(AllocError::OutOfChunks);
        }

        // Reserve chunks with zero length — the writer fills in actual
        // byte counts as data arrives.
        let mut reserved = 0;
        for idx in 0..CHUNK_COUNT {
            if reserved >= chunks_needed {
                break;
            }
            if self.owners[idx].is_none() {
                self.owners[idx] = Some((allocation_id, 0));
                reserved += 1;
            }
        }

        Ok(ChunkWriter {
            allocator: self,
            allocation_id,
            chunk_cursor: 0,
            byte_offset: 0,
            total_remaining: len,
        })
    }

    /// Returns an iterator over the bytes belonging to `allocation_id`, in chunk order.
    pub fn iter(&self, allocation_id: u32) -> Result<AllocIter<'_, 'a, CHUNK_SIZE, CHUNK_COUNT>, AllocError> {
        let has_any = self
            .owners
            .iter()
            .any(|o| matches!(o, Some((id, _)) if *id == allocation_id));

        if !has_any {
            return Err(AllocError::UnknownAllocation);
        }

        Ok(AllocIter {
            allocator: self,
            allocation_id,
            chunk_idx: 0,
            byte_idx: 0,
        })
    }
}

pub struct AllocIter<'b, 'a, const CHUNK_SIZE: usize, const CHUNK_COUNT: usize> {
    allocator: &'b Allocator<'a, CHUNK_SIZE, CHUNK_COUNT>,
    allocation_id: u32,
    chunk_idx: usize,
    byte_idx: usize,
}

impl<const CHUNK_SIZE: usize, const CHUNK_COUNT: usize> Iterator
    for AllocIter<'_, '_, CHUNK_SIZE, CHUNK_COUNT>
{
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        while self.chunk_idx < CHUNK_COUNT {
            match self.allocator.owners[self.chunk_idx] {
                Some((id, len)) if id == self.allocation_id => {
                    if self.byte_idx < len {
                        // Safety: `len` bytes in this chunk were initialized by `write`.
                        let b = unsafe { self.allocator.chunk_initialized(self.chunk_idx, len) }[self.byte_idx];
                        self.byte_idx += 1;
                        return Some(b);
                    }
                    self.chunk_idx += 1;
                    self.byte_idx = 0;
                }
                _ => {
                    self.chunk_idx += 1;
                    self.byte_idx = 0;
                }
            }
        }
        None
    }
}

pub struct ChunkWriter<'b, 'a, const CHUNK_SIZE: usize, const CHUNK_COUNT: usize> {
    allocator: &'b mut Allocator<'a, CHUNK_SIZE, CHUNK_COUNT>,
    allocation_id: u32,
    /// Next index in `owners` to scan from when looking for our chunks.
    chunk_cursor: usize,
    /// Byte offset within the current chunk.
    byte_offset: usize,
    /// Bytes remaining before the reservation is full.
    total_remaining: usize,
}

impl<const CHUNK_SIZE: usize, const CHUNK_COUNT: usize>
    ChunkWriter<'_, '_, CHUNK_SIZE, CHUNK_COUNT>
{
    /// Advance `chunk_cursor` to the next chunk owned by this allocation.
    /// Returns `true` if one was found.
    fn advance_to_next_chunk(&mut self) -> bool {
        while self.chunk_cursor < CHUNK_COUNT {
            if matches!(self.allocator.owners[self.chunk_cursor], Some((id, _)) if id == self.allocation_id) {
                return true;
            }
            self.chunk_cursor += 1;
        }
        false
    }

    pub fn append(&mut self, data: &[u8]) -> Result<(), AllocError> {
        if data.len() > self.total_remaining {
            return Err(AllocError::Overflow);
        }

        let mut remaining = data;
        while !remaining.is_empty() {
            if !self.advance_to_next_chunk() {
                return Err(AllocError::Overflow);
            }

            let space = CHUNK_SIZE - self.byte_offset;
            let take = remaining.len().min(space);

            // Safety: writing initialized bytes into MaybeUninit storage.
            unsafe {
                let dst = self.allocator.chunk_mut(self.chunk_cursor)
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(self.byte_offset);
                core::ptr::copy_nonoverlapping(remaining.as_ptr(), dst, take);
            }

            // Update the initialized length for this chunk.
            self.allocator.owners[self.chunk_cursor] =
                Some((self.allocation_id, self.byte_offset + take));

            remaining = &remaining[take..];
            self.byte_offset += take;
            self.total_remaining -= take;

            if self.byte_offset == CHUNK_SIZE {
                self.chunk_cursor += 1;
                self.byte_offset = 0;
            }
        }

        Ok(())
    }
}
