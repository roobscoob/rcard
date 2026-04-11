use crate::error::HeaderError;
use crate::frame::RawFrame;
use crate::header::{FrameHeader, HEADER_SIZE};

/// Errors from [`FrameReader::next_frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderError {
    /// The frame header is malformed.
    Header(HeaderError),
    /// The frame's declared size exceeds the buffer capacity — it can
    /// never be assembled.
    ///
    /// Call [`FrameReader::skip_frame()`] with the declared size to
    /// discard the frame's bytes and re-synchronize, or
    /// [`FrameReader::reset()`] to drop everything.
    Oversized {
        /// The frame's total declared size (header + payload).
        declared_size: usize,
    },
}

impl From<HeaderError> for ReaderError {
    fn from(e: HeaderError) -> Self {
        Self::Header(e)
    }
}

#[cfg(feature = "std")]
impl core::fmt::Display for ReaderError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Header(e) => write!(f, "{}", e),
            Self::Oversized { declared_size } => {
                write!(f, "frame declares {} bytes, exceeds buffer", declared_size)
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ReaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Header(e) => Some(e),
            Self::Oversized { .. } => None,
        }
    }
}

/// Incrementally assemble frames from a byte stream.
///
/// Feed chunks from USB bulk reads into [`push()`](Self::push), then
/// drain complete frames with [`next_frame()`](Self::next_frame).
pub struct FrameReader<const N: usize = 4096> {
    buf: [u8; N],
    len: usize,
    /// Bytes still to discard before normal parsing resumes.
    skip_remaining: usize,
}

impl<const N: usize> FrameReader<N> {
    const _ASSERT_MIN_SIZE: () = assert!(N >= HEADER_SIZE, "buffer must fit at least one header");

    pub const fn new() -> Self {
        #[allow(clippy::let_unit_value)]
        let _ = Self::_ASSERT_MIN_SIZE;
        Self {
            buf: [0; N],
            len: 0,
            skip_remaining: 0,
        }
    }

    /// Append raw bytes from a USB read.
    ///
    /// If the reader is in skip mode (after [`skip_frame()`](Self::skip_frame)),
    /// incoming bytes are silently discarded until the oversized frame has
    /// been fully consumed.
    ///
    /// Returns the number of bytes consumed from `data`.
    pub fn push(&mut self, data: &[u8]) -> usize {
        // If skipping, discard bytes before buffering.
        if self.skip_remaining > 0 {
            let discard = data.len().min(self.skip_remaining);
            self.skip_remaining -= discard;
            // Recurse with the remainder (if any) to buffer normally.
            if discard < data.len() {
                return discard + self.push(&data[discard..]);
            }
            return discard;
        }

        let space = N - self.len;
        let n = data.len().min(space);
        self.buf[self.len..self.len + n].copy_from_slice(&data[..n]);
        self.len += n;
        n
    }

    /// Try to decode the next complete frame.
    ///
    /// The returned [`RawFrame`] borrows from the internal buffer and
    /// is valid until [`consume()`](Self::consume) is called.
    ///
    /// Returns `Err(ReaderError::Oversized { .. })` if the frame's
    /// declared size exceeds the buffer capacity. Call
    /// [`skip_frame()`](Self::skip_frame) with the returned
    /// `declared_size` to discard the frame and re-synchronize.
    pub fn next_frame(&self) -> Result<Option<RawFrame<'_>>, ReaderError> {
        if self.skip_remaining > 0 {
            return Ok(None);
        }
        if self.len < HEADER_SIZE {
            return Ok(None);
        }
        let header = FrameHeader::decode(&self.buf[..self.len])?;
        let total = header.frame_size();
        if total > N {
            return Err(ReaderError::Oversized {
                declared_size: total,
            });
        }
        if self.len < total {
            return Ok(None);
        }
        Ok(Some(RawFrame {
            header,
            payload: &self.buf[HEADER_SIZE..total],
        }))
    }

    /// Begin skipping an oversized frame.
    ///
    /// Pass the `declared_size` from [`ReaderError::Oversized`]. The
    /// reader discards any already-buffered bytes belonging to the frame
    /// and enters skip mode — subsequent [`push()`](Self::push) calls
    /// silently discard bytes until the entire declared frame has passed.
    pub fn skip_frame(&mut self, declared_size: usize) {
        if declared_size <= self.len {
            // Entire frame (or more) is already buffered — just consume it.
            self.consume(declared_size);
        } else {
            // We have the header + partial payload in the buffer.
            // Discard what we have, track the remainder.
            self.skip_remaining = declared_size - self.len;
            self.len = 0;
        }
    }

    /// Remove the first `frame_size` bytes from the buffer.
    ///
    /// Call this after [`next_frame()`] returns `Some`, passing
    /// `header.frame_size()`.
    pub fn consume(&mut self, frame_size: usize) {
        let n = frame_size.min(self.len);
        if n >= self.len {
            self.len = 0;
        } else {
            self.buf.copy_within(n..self.len, 0);
            self.len -= n;
        }
    }

    /// Discard all buffered data and cancel any in-progress skip.
    pub fn reset(&mut self) {
        self.len = 0;
        self.skip_remaining = 0;
    }

    /// Number of bytes currently buffered.
    pub fn buffered(&self) -> usize {
        self.len
    }

    /// Whether the reader is discarding bytes from an oversized frame.
    pub fn is_skipping(&self) -> bool {
        self.skip_remaining > 0
    }
}
