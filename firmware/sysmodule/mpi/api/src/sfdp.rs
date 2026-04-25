//! SFDP (Serial Flash Discoverable Parameters, JESD216) body parsers.
//!
//! The IPC trait gives clients raw bytes:
//!   - `read_sfdp(lease)` writes the parameter-header table — N slots of
//!     8 bytes each, in on-wire format.
//!   - `read_parameter(id, index, lease)` writes one parameter table
//!     body — a sequence of little-endian 32-bit DWORDs whose meaning
//!     is parameter-specific.
//!
//! This module decodes both. [`Bfpt`] wraps a BFPT body (parameter ID
//! `0xFF00`) with zero-copy accessors for every field driver code
//! typically wants. [`parse_parameter_headers`] yields
//! [`ParameterHeader`]s from a [`read_sfdp`]-style lease output.

use crate::ParameterHeader;
use crate::ParameterId;
use crate::SfdpHeader;

/// Address-byte widths a chip advertises in BFPT DWORD 1 bits 18:17.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    rcard_log::Format,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum AddrBytesSupport {
    /// Chip only accepts 3-byte addresses (up to 16 MiB addressable).
    ThreeOnly = 0,
    /// Chip accepts both 3- and 4-byte addresses. EN4B/EX4B switches.
    ThreeOrFour = 1,
    /// Chip only accepts 4-byte addresses.
    FourOnly = 2,
    /// Reserved BFPT encoding (should not appear on compliant parts).
    Reserved = 3,
}

/// Per-mode fast-read parameters packed into DWORDs 3 and 4 of BFPT.
/// The `opcode`, `dummy_cycles`, and `mode_cycles` together tell the
/// driver how to issue one of the 1-1-2 / 1-2-2 / 1-1-4 / 1-4-4 reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, rcard_log::Format)]
pub struct FastReadParams {
    pub opcode: u8,
    /// 0-31. The number of wait cycles between address and data.
    pub dummy_cycles: u8,
    /// Typically 0 or 2. Number of mode-byte cycles before dummy.
    /// Mode bits gate continuous-read mode (CRM) on some chips.
    pub mode_cycles: u8,
}

impl FastReadParams {
    /// Decode the 16-bit packed triple used throughout DWORDs 3-4.
    fn from_bits(bits: u16) -> Self {
        Self {
            dummy_cycles: (bits & 0x1F) as u8,
            mode_cycles: ((bits >> 5) & 0x07) as u8,
            opcode: (bits >> 8) as u8,
        }
    }
}

/// One of the chip's supported erase types from BFPT DWORDs 8-9. Slots
/// whose on-wire `size` byte is zero are unused; those come back as
/// `None` from [`Bfpt::erase_types`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, rcard_log::Format)]
pub struct EraseType {
    /// Always a power of two.
    pub size_bytes: u32,
    pub opcode: u8,
}

/// Quad Enable Requirement, from BFPT rev-B+ DWORD 15 bits 23:20. Tells
/// the driver where the chip's QE ("quad enable") bit lives and how it
/// must be written to permit quad-mode reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, rcard_log::Format)]
pub enum QuadEnableRequirement {
    /// QE not defined / chip already in quad-capable state.
    None,
    /// QE = SR2 bit 1. Write SR2 via the 1-byte WRSR2 opcode (0x31).
    Sr2Bit1Wrsr2,
    /// QE = SR1 bit 6. Write via WRSR (0x01), 1 byte.
    Sr1Bit6,
    /// QE = SR2 bit 7. Write via WRSR2 (0x3E), 1 byte.
    Sr2Bit7,
    /// QE = SR2 bit 1. Write via WRSR (0x01), 2-byte payload (SR1 then SR2).
    Sr2Bit1Wrsr2Byte,
    /// QE = SR2 bit 1. Mixed: some chips require reading SR2 via 0x35,
    /// then writing both SR1 and SR2 back via WRSR (0x01, 2 bytes), and
    /// verifying via RDSR2.
    Sr2Bit1WrsrMixed,
    /// QE = SR2 bit 1 via WRSR2 (0x31), 1 byte. Same location as
    /// `Sr2Bit1Wrsr2` but different recommended write sequence (status
    /// polling after WREN).
    Sr2Bit1Wrsr2Alt,
    /// Reserved / unknown encoding.
    Reserved,
}

impl EraseType {
    fn from_bits(bits: u16) -> Option<Self> {
        let size_log2 = (bits & 0xFF) as u8;
        // A size-log2 of 0 means "no erase type here" per JESD216, not
        // "1-byte erase". Filter it out.
        if size_log2 == 0 {
            return None;
        }
        Some(Self {
            size_bytes: 1u32 << size_log2,
            opcode: (bits >> 8) as u8,
        })
    }
}

/// Zero-copy view over a BFPT (Basic Flash Parameter Table) body. Wrap
/// the lease bytes returned by `Mpi::read_parameter(ParameterId::BFPT,
/// 0, lease)` and call accessors on demand — each one reads only the
/// DWORDs it needs.
///
/// Accessors are total over short buffers: if the DWORD a field lives
/// in isn't present (rev-A chips populate only 9 DWORDs, rev-B 16,
/// rev-F up to 23), the accessor returns `None` rather than panicking.
pub struct Bfpt<'a>(&'a [u8]);

impl<'a> Bfpt<'a> {
    /// Wrap a BFPT body byte slice. Does not copy.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    /// Raw access to the backing bytes, for callers who need to peek at
    /// DWORDs not yet surfaced by typed accessors.
    pub fn as_bytes(&self) -> &'a [u8] {
        self.0
    }

    /// Read DWORD `idx` (0-based) as a little-endian u32. Returns `None`
    /// if the DWORD is past the end of the buffer.
    fn dword(&self, idx: usize) -> Option<u32> {
        let start = idx * 4;
        let end = start + 4;
        if end > self.0.len() {
            return None;
        }
        Some(u32::from_le_bytes([
            self.0[start],
            self.0[start + 1],
            self.0[start + 2],
            self.0[start + 3],
        ]))
    }

    // --- DWORD 1: erase + addr modes + fast-read flags ---

    /// 4 KB sector erase opcode, if the chip advertises 4 KB erase
    /// (DWORD 1 bits 1:0 == 0b01). `None` means the chip doesn't
    /// support 4 KB erase and you'd need to use a larger granularity.
    pub fn sector_erase_4k_opcode(&self) -> Option<u8> {
        let dw1 = self.dword(0)?;
        if dw1 & 0b11 == 0b01 {
            Some((dw1 >> 8) as u8)
        } else {
            None
        }
    }

    /// Which address widths the chip accepts. Drives EN4B/EX4B decision
    /// at `open()` time.
    pub fn address_bytes(&self) -> Option<AddrBytesSupport> {
        let dw1 = self.dword(0)?;
        Some(match (dw1 >> 17) & 0b11 {
            0b00 => AddrBytesSupport::ThreeOnly,
            0b01 => AddrBytesSupport::ThreeOrFour,
            0b10 => AddrBytesSupport::FourOnly,
            _ => AddrBytesSupport::Reserved,
        })
    }

    /// DTR (double-data-rate) clocking advertised. Independent of any
    /// specific fast-read mode — if set, the chip can do DTR on all its
    /// advertised fast-read modes.
    pub fn supports_dtr(&self) -> bool {
        self.dword(0).map(|dw| (dw >> 19) & 1 == 1).unwrap_or(false)
    }

    /// Dual Output Fast Read (1-1-2). Data phase uses 2 lanes;
    /// instruction and address stay single-line. Returns `None` if the
    /// chip doesn't advertise this mode.
    pub fn dual_output_read(&self) -> Option<FastReadParams> {
        let dw1 = self.dword(0)?;
        if (dw1 >> 16) & 1 == 0 {
            return None;
        }
        // 1-1-2 triple lives in the low 16 bits of DWORD 4.
        let dw4 = self.dword(3)?;
        Some(FastReadParams::from_bits(dw4 as u16))
    }

    /// Dual I/O Fast Read (1-2-2). Address + data both on 2 lanes.
    pub fn dual_io_read(&self) -> Option<FastReadParams> {
        let dw1 = self.dword(0)?;
        if (dw1 >> 20) & 1 == 0 {
            return None;
        }
        // 1-2-2 triple lives in the high 16 bits of DWORD 4.
        let dw4 = self.dword(3)?;
        Some(FastReadParams::from_bits((dw4 >> 16) as u16))
    }

    /// Quad Output Fast Read (1-1-4). Data phase uses 4 lanes.
    pub fn quad_output_read(&self) -> Option<FastReadParams> {
        let dw1 = self.dword(0)?;
        if (dw1 >> 22) & 1 == 0 {
            return None;
        }
        // 1-1-4 triple lives in the high 16 bits of DWORD 3.
        let dw3 = self.dword(2)?;
        Some(FastReadParams::from_bits((dw3 >> 16) as u16))
    }

    /// Quad I/O Fast Read (1-4-4). Address + data on 4 lanes; the
    /// fastest mode most chips support. Some chips also use the
    /// mode-byte cycles here to enter continuous-read mode (CRM),
    /// which skips the instruction byte on follow-up reads.
    pub fn quad_io_read(&self) -> Option<FastReadParams> {
        let dw1 = self.dword(0)?;
        if (dw1 >> 21) & 1 == 0 {
            return None;
        }
        // 1-4-4 triple lives in the low 16 bits of DWORD 3.
        let dw3 = self.dword(2)?;
        Some(FastReadParams::from_bits(dw3 as u16))
    }

    // --- DWORD 2: density ---

    /// Chip capacity in bytes, decoded from DWORD 2. Two encodings:
    ///   - bit 31 = 0 → `density_bits = dw2 + 1` (direct)
    ///   - bit 31 = 1 → `density_bits = 1 << (dw2 & 0x7FFF_FFFF)` (log)
    /// The log form's exponent must be 3..=63.
    pub fn density_bytes(&self) -> Option<u64> {
        let dw2 = self.dword(1)?;
        if dw2 & (1 << 31) == 0 {
            Some((dw2 as u64 + 1) / 8)
        } else {
            let n = dw2 & 0x7FFF_FFFF;
            if !(3..=63).contains(&n) {
                return None;
            }
            Some(1u64 << (n - 3))
        }
    }

    // --- DWORDs 8-9: erase types ---

    /// Up to 4 erase types the chip supports, in the order they appear
    /// in BFPT. A slot is `None` if (a) the DWORD holding it isn't
    /// present in the buffer, or (b) the on-wire size byte was 0
    /// (unused slot).
    pub fn erase_types(&self) -> [Option<EraseType>; 4] {
        let dw8 = self.dword(7);
        let dw9 = self.dword(8);
        [
            dw8.and_then(|v| EraseType::from_bits(v as u16)),
            dw8.and_then(|v| EraseType::from_bits((v >> 16) as u16)),
            dw9.and_then(|v| EraseType::from_bits(v as u16)),
            dw9.and_then(|v| EraseType::from_bits((v >> 16) as u16)),
        ]
    }

    // --- Rev B+ fields (DWORDs 11..) ---

    /// Page program size in bytes, decoded from DWORD 11 bits 7:4 as
    /// `1 << n`. Typical value is 256. Returns `None` on rev-A chips
    /// (only 9 DWORDs) since DWORD 11 isn't present — caller should
    /// assume 256 as a default.
    pub fn page_size(&self) -> Option<u32> {
        let dw11 = self.dword(10)?;
        let n = ((dw11 >> 4) & 0xF) as u32;
        // n = 0 encodes "not specified" rather than a 1-byte page.
        if n == 0 {
            return None;
        }
        Some(1u32 << n)
    }

    /// Quad Enable Requirement — how to tell the chip it's OK to
    /// respond to quad-mode reads. Read from DWORD 15 bits 23:20 on
    /// rev-B+ chips. Returns `None` if DWORD 15 isn't present (rev A);
    /// caller should default to `Sr2Bit1Wrsr2Byte` (the most common
    /// modern encoding) or refuse to enable quad modes.
    pub fn quad_enable_requirement(&self) -> Option<QuadEnableRequirement> {
        let dw15 = self.dword(14)?;
        Some(match (dw15 >> 20) & 0b111 {
            0b000 => QuadEnableRequirement::None,
            0b001 => QuadEnableRequirement::Sr2Bit1Wrsr2,
            0b010 => QuadEnableRequirement::Sr1Bit6,
            0b011 => QuadEnableRequirement::Sr2Bit7,
            0b100 => QuadEnableRequirement::Sr2Bit1Wrsr2Byte,
            0b101 => QuadEnableRequirement::Sr2Bit1WrsrMixed,
            0b110 => QuadEnableRequirement::Sr2Bit1Wrsr2Alt,
            _ => QuadEnableRequirement::Reserved,
        })
    }
}

/// Bundle of the SFDP global header and a zero-copy [`Bfpt`] view over
/// BFPT body bytes. The usual pattern: call `read_sfdp` to get the
/// header, call `read_parameter(ParameterId::BFPT, 0, ..)` to get the
/// body, then stuff both here so higher-layer code passes around one
/// value instead of juggling the pair.
pub struct Sfdp<'a> {
    pub header: SfdpHeader,
    pub bfpt: Bfpt<'a>,
}

impl<'a> Sfdp<'a> {
    /// Construct from the global header and a BFPT body byte slice.
    /// Does not copy.
    pub fn new(header: SfdpHeader, bfpt_bytes: &'a [u8]) -> Self {
        Self {
            header,
            bfpt: Bfpt::new(bfpt_bytes),
        }
    }
}

// --- IPC client helpers (only available on the firmware target) ---
#[cfg(target_os = "none")]
mod ipc_ext {
    use super::*;
    use crate::mpi_client::{MpiHandle, MpiServer};
    use crate::ReadParameterError;
    use crate::ReadSfdpError;
    use ipc::errors::HandleLostError;

    /// Error type returned by [`MpiExt`] helpers — bundles failures from
    /// the underlying `read_sfdp`/`read_parameter` calls plus "BFPT
    /// missing" (malformed SFDP layout the spec doesn't strictly forbid).
    #[derive(Debug, Clone, Copy, rcard_log::Format)]
    pub enum WithSfdpError {
        /// IPC transport failed — server died or handle was lost.
        HandleLost,
        /// `read_sfdp` couldn't serve state — open() should have failed
        /// first, so this is defensive.
        SfdpUnavailable,
        /// `read_parameter` returned an error during a body fetch.
        ReadParameter(ReadParameterError),
        /// No parameter header with ID `0xFF00` (BFPT) exists on this
        /// chip. Spec requires BFPT but we verify rather than assume.
        BfptMissing,
        /// BFPT body declared larger than the driver's staging limit.
        /// Unlikely on any real chip (max real-world BFPT is ~92 bytes).
        BufferTooSmall,
    }

    impl From<HandleLostError> for WithSfdpError {
        fn from(_: HandleLostError) -> Self {
            WithSfdpError::HandleLost
        }
    }

    impl From<ReadSfdpError> for WithSfdpError {
        fn from(e: ReadSfdpError) -> Self {
            match e {
                ReadSfdpError::SfdpUnavailable => WithSfdpError::SfdpUnavailable,
            }
        }
    }

    impl From<ReadParameterError> for WithSfdpError {
        fn from(e: ReadParameterError) -> Self {
            WithSfdpError::ReadParameter(e)
        }
    }

    /// Ergonomic extensions on top of the raw IPC methods — avoids the
    /// boilerplate of "allocate a buffer, call `read_sfdp`, parse the PH
    /// table, find BFPT, allocate another buffer, call `read_parameter`,
    /// wire up the lifetimes" every time you need SFDP data.
    ///
    /// Implemented for [`MpiHandle<S>`] for any server backend `S`. Callers
    /// typically get this automatically after `use sysmodule_mpi_api::sfdp::MpiExt;`.
    pub trait MpiExt {
        /// Read the SFDP header + BFPT body, bundle them into [`Sfdp`], and
        /// call `f`. Internal buffers are stack-allocated; no heap.
        ///
        /// Returns whatever the closure returns, wrapped in a Result for
        /// the cases where the SFDP/BFPT fetch itself fails.
        fn with_sfdp<T, F>(&self, f: F) -> Result<T, WithSfdpError>
        where
            F: for<'a> FnOnce(Sfdp<'a>) -> T;

        /// Iterate every parameter table in SFDP. For each slot in the
        /// parameter-header table, fetches the body into `body_buf` and
        /// invokes `f(parameter_header, body_bytes)` — the closure's
        /// return is yielded as the iterator item.
        ///
        /// `body_buf` is reused across iterations: it's refilled on every
        /// `.next()`. If a parameter body is larger than `body_buf`, the
        /// bytes are truncated — check the `length_dwords` on the header
        /// to size accordingly. If a fetch fails, the iterator yields
        /// `Err(_)` but keeps going; pass `?` or a `collect()` to stop on
        /// first error.
        ///
        /// The closure receives bytes whose lifetime is only valid until
        /// the next `.next()` call — hence the callback shape rather than
        /// returning borrowed slices directly (the standard `Iterator`
        /// trait can't express lending iterators).
        fn map_sfdp_parameter<'m, 'b, T, F>(
            &'m self,
            body_buf: &'b mut [u8],
            f: F,
        ) -> SfdpParameterIter<'m, 'b, Self, F>
        where
            F: FnMut(ParameterHeader, &[u8]) -> T,
            'm: 'b;
    }

    /// Stream of `(ParameterHeader, body_bytes) → T` invocations. Yields
    /// one item per parameter header in SFDP order; the body is fetched
    /// lazily on each `.next()` call.
    pub struct SfdpParameterIter<'m, 'b, M: ?Sized, F> {
        mpi: &'m M,
        headers: [u8; 16 * 8],
        nph: u16,
        idx: u16,
        body_buf: &'b mut [u8],
        f: F,
        pending_err: Option<WithSfdpError>,
    }

    impl<S: MpiServer> MpiExt for MpiHandle<S> {
        fn with_sfdp<T, F>(&self, f: F) -> Result<T, WithSfdpError>
        where
            F: for<'a> FnOnce(Sfdp<'a>) -> T,
        {
            // Server caps at 16 parameter headers × 8 bytes = 128.
            // Each IPC call returns Result<Result<T, E>, HandleLostError>,
            // so `??` flattens transport + server-side errors via our
            // `From` impls for `WithSfdpError`.
            let mut ph_buf = [0u8; 16 * 8];
            let header = self.read_sfdp(&mut ph_buf)??;

            // BFPT is spec-required to be PH index 0, but search by ID so
            // out-of-order SFDPs don't brick us.
            let bfpt_ph = parse_parameter_headers(&ph_buf, header.nph)
                .find(|ph| ph.id == ParameterId::BFPT)
                .ok_or(WithSfdpError::BfptMissing)?;

            // Cap BFPT at 256 bytes (matching server-side limit).
            let body_bytes = (bfpt_ph.length_dwords as usize) * 4;
            if body_bytes > 256 {
                return Err(WithSfdpError::BufferTooSmall);
            }
            let mut bfpt_buf = [0u8; 256];
            let bfpt_lease = &mut bfpt_buf[..body_bytes];
            let _meta = self
                .read_parameter(ParameterId::BFPT, 0, bfpt_lease)??
                .ok_or(WithSfdpError::BfptMissing)?;

            let sfdp = Sfdp::new(header, bfpt_lease);
            Ok(f(sfdp))
        }

        fn map_sfdp_parameter<'m, 'b, T, F>(
            &'m self,
            body_buf: &'b mut [u8],
            f: F,
        ) -> SfdpParameterIter<'m, 'b, Self, F>
        where
            F: FnMut(ParameterHeader, &[u8]) -> T,
            'm: 'b,
        {
            let mut headers = [0u8; 16 * 8];
            let (nph, pending_err) = match self.read_sfdp(&mut headers) {
                Ok(Ok(h)) => (h.nph, None),
                Ok(Err(e)) => (0, Some(WithSfdpError::from(e))),
                Err(e) => (0, Some(WithSfdpError::from(e))),
            };
            SfdpParameterIter {
                mpi: self,
                headers,
                nph,
                idx: 0,
                body_buf,
                f,
                pending_err,
            }
        }
    }

    impl<'m, 'b, S: MpiServer, T, F> Iterator for SfdpParameterIter<'m, 'b, MpiHandle<S>, F>
    where
        F: FnMut(ParameterHeader, &[u8]) -> T,
    {
        type Item = Result<T, WithSfdpError>;

        fn next(&mut self) -> Option<Self::Item> {
            if let Some(e) = self.pending_err.take() {
                return Some(Err(e));
            }
            if self.idx >= self.nph {
                return None;
            }

            // Decode this slot's header.
            let off = self.idx as usize * 8;
            let s = &self.headers[off..off + 8];
            let id = ParameterId(((s[7] as u16) << 8) | (s[0] as u16));
            let ph = ParameterHeader {
                id,
                minor: s[1],
                major: s[2],
                length_dwords: s[3],
                pointer: u32::from_le_bytes([s[4], s[5], s[6], 0]),
            };

            // `read_parameter` keys duplicates by (id, index) — compute the
            // 0-based occurrence count of this ID in prior slots.
            let mut index: u8 = 0;
            for i in 0..self.idx as usize {
                let prior = &self.headers[i * 8..i * 8 + 8];
                let prior_id = ParameterId(((prior[7] as u16) << 8) | (prior[0] as u16));
                if prior_id == id {
                    index = index.saturating_add(1);
                }
            }

            self.idx += 1;

            match self.mpi.read_parameter(id, index, &mut self.body_buf[..]) {
                Ok(Ok(Some(meta))) => {
                    let n =
                        ((meta.header.length_dwords as usize) * 4).min(self.body_buf.len());
                    let t = (self.f)(ph, &self.body_buf[..n]);
                    Some(Ok(t))
                }
                // Shouldn't happen — we just scanned the PH table and know
                // this slot exists. Surface defensively.
                Ok(Ok(None)) => Some(Err(WithSfdpError::BfptMissing)),
                Ok(Err(e)) => Some(Err(WithSfdpError::ReadParameter(e))),
                Err(e) => Some(Err(WithSfdpError::from(e))),
            }
        }
    }
}

#[cfg(target_os = "none")]
pub use ipc_ext::*;

/// Iterator over the 8-byte parameter-header slots returned by
/// `Mpi::read_sfdp`. Stops at `nph` entries or end-of-buffer, whichever
/// comes first — short leases are handled gracefully.
pub fn parse_parameter_headers(
    bytes: &[u8],
    nph: u16,
) -> impl Iterator<Item = ParameterHeader> + '_ {
    const SLOT: usize = 8;
    let max_from_bytes = bytes.len() / SLOT;
    let count = (nph as usize).min(max_from_bytes);
    (0..count).map(move |i| {
        let s = &bytes[i * SLOT..(i + 1) * SLOT];
        ParameterHeader {
            id: ParameterId(((s[7] as u16) << 8) | (s[0] as u16)),
            minor: s[1],
            major: s[2],
            length_dwords: s[3],
            pointer: u32::from_le_bytes([s[4], s[5], s[6], 0]),
        }
    })
}
