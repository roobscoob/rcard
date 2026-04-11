#![no_std]

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
)]
#[repr(u8)]
pub enum UsbError {
    AlreadyTaken = 0,
    NotConfigured = 1,
    InvalidEndpoint = 2,
    EndpointBusy = 3,
    Stalled = 4,
    Disconnected = 5,
    BufferOverflow = 6,
    MalformedIdentity = 7,
}

/// Builder-side errors for [`DeviceIdentityBuilder`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, rcard_log::Format)]
pub enum BuilderError {
    /// The scratch buffer passed to `DeviceIdentity::builder` is too small
    /// to hold all the accumulated strings and capability descriptors.
    BufferTooSmall,
    /// A single entry exceeded 65535 bytes (TLV length field is u16).
    EntryTooLong,
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
)]
#[repr(u8)]
pub enum BusState {
    /// Not attached to the bus.
    Detached = 0,
    /// Attached, waiting for host reset.
    Powered = 1,
    /// Reset received, default address.
    Default = 2,
    /// SET_ADDRESS completed.
    Addressed = 3,
    /// SET_CONFIGURATION completed, endpoints active.
    Configured = 4,
    /// Host suspended the device.
    Suspended = 5,
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
)]
#[repr(u8)]
pub enum Direction {
    /// Host → device.
    Out = 0,
    /// Device → host.
    In = 1,
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
)]
#[repr(u8)]
pub enum TransferType {
    Bulk = 0,
    Interrupt = 1,
    Isochronous = 2,
}

// ---------------------------------------------------------------------------
// Fixed device config — the small, Copy, wire-safe half of DeviceIdentity.
// Everything variable-length (strings, BOS capabilities, vendor request
// responses) rides alongside in a TLV blob sent as an IPC lease.
// ---------------------------------------------------------------------------

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::FromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
)]
#[repr(C, packed)]
pub struct FixedDeviceConfig {
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub bcd_device: u16,
}

// ---------------------------------------------------------------------------
// TLV blob format
//
// The identity blob is a flat sequence of entries, each laid out as:
//   tag:   u8
//   len:   u16 (little-endian)
//   data:  [u8; len]
//
// Tags are defined below. Unknown tags are skipped by the parser, so forward-
// compatible additions are cheap.
// ---------------------------------------------------------------------------

const TAG_MANUFACTURER: u8 = 0x01;
const TAG_PRODUCT: u8 = 0x02;
const TAG_SERIAL: u8 = 0x03;
/// BOS device capability descriptor. Data layout: `[cap_type, ...payload]`.
/// `cap_type` is the `bDevCapabilityType` byte (e.g. 0x05 = Platform); the
/// server writes `bLength`/`bDescriptorType`/`cap_type`/`payload` using
/// usb-device's `BosWriter::capability`.
const TAG_BOS_CAPABILITY: u8 = 0x10;
/// Vendor-specific control IN handler. Data layout:
/// `[bRequest, wIndex_lo, wIndex_hi, ...response_bytes]`. The server matches
/// on `(bRequest, wIndex)` and returns the response bytes via
/// `ControlIn::accept_with`.
const TAG_VENDOR_REQUEST: u8 = 0x20;

/// Identity produced by [`DeviceIdentityBuilder::build`]. Borrows the
/// caller's scratch buffer for the blob half.
pub struct DeviceIdentity<'a> {
    config: FixedDeviceConfig,
    blob: &'a [u8],
}

impl<'a> DeviceIdentity<'a> {
    /// Start building an identity into the caller-supplied scratch buffer.
    /// The buffer must live at least as long as the identity is used.
    /// A 2 KiB buffer is plenty for realistic devices (a basic MSOS-enabled
    /// setup with strings uses ~130 bytes).
    pub fn builder(
        buf: &'a mut [u8],
        vendor_id: u16,
        product_id: u16,
    ) -> DeviceIdentityBuilder<'a> {
        DeviceIdentityBuilder {
            buf,
            used: 0,
            config: FixedDeviceConfig {
                vendor_id,
                product_id,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                bcd_device: 0x0100,
            },
            error: None,
        }
    }

    pub fn config(&self) -> FixedDeviceConfig {
        self.config
    }

    pub fn blob(&self) -> &'a [u8] {
        self.blob
    }
}

pub struct DeviceIdentityBuilder<'a> {
    buf: &'a mut [u8],
    used: usize,
    config: FixedDeviceConfig,
    error: Option<BuilderError>,
}

impl<'a> DeviceIdentityBuilder<'a> {
    fn write_entry(&mut self, tag: u8, parts: &[&[u8]]) {
        if self.error.is_some() {
            return;
        }
        let total: usize = parts.iter().map(|p| p.len()).sum();
        if total > u16::MAX as usize {
            self.error = Some(BuilderError::EntryTooLong);
            return;
        }
        let entry_len = 3 + total;
        if self.used + entry_len > self.buf.len() {
            self.error = Some(BuilderError::BufferTooSmall);
            return;
        }
        self.buf[self.used] = tag;
        let len = total as u16;
        self.buf[self.used + 1] = len as u8;
        self.buf[self.used + 2] = (len >> 8) as u8;
        let mut off = self.used + 3;
        for part in parts {
            self.buf[off..off + part.len()].copy_from_slice(part);
            off += part.len();
        }
        self.used += entry_len;
    }

    pub fn device_class(mut self, class: u8, subclass: u8, protocol: u8) -> Self {
        self.config.device_class = class;
        self.config.device_subclass = subclass;
        self.config.device_protocol = protocol;
        self
    }

    pub fn device_release(mut self, bcd: u16) -> Self {
        self.config.bcd_device = bcd;
        self
    }

    pub fn manufacturer(mut self, s: &str) -> Self {
        self.write_entry(TAG_MANUFACTURER, &[s.as_bytes()]);
        self
    }

    pub fn product(mut self, s: &str) -> Self {
        self.write_entry(TAG_PRODUCT, &[s.as_bytes()]);
        self
    }

    pub fn serial(mut self, s: &str) -> Self {
        self.write_entry(TAG_SERIAL, &[s.as_bytes()]);
        self
    }

    /// Append a Device Capability descriptor to the BOS.
    ///
    /// `cap_type` is the `bDevCapabilityType` byte from the USB spec
    /// (e.g. 0x05 = Platform Capability). `payload` is the capability-specific
    /// data — everything after `bDevCapabilityType`. `bLength` and
    /// `bDescriptorType` are added by the server.
    pub fn bos_capability(mut self, cap_type: u8, payload: &[u8]) -> Self {
        self.write_entry(TAG_BOS_CAPABILITY, &[&[cap_type], payload]);
        self
    }

    /// Register a vendor-specific control IN handler.
    ///
    /// When the host issues a control IN with `bmRequestType = Vendor`,
    /// `bRequest = code`, and `wIndex = index`, the server replies with
    /// `response`. MSOS 2.0 uses `index = 0x07` (MS_OS_20_DESCRIPTOR_INDEX).
    pub fn vendor_request(mut self, code: u8, index: u16, response: &[u8]) -> Self {
        let idx = index.to_le_bytes();
        self.write_entry(TAG_VENDOR_REQUEST, &[&[code], &idx, response]);
        self
    }

    /// Finalize the identity. Returns an error if the scratch buffer
    /// overflowed at any point during construction.
    pub fn build(self) -> Result<DeviceIdentity<'a>, BuilderError> {
        if let Some(e) = self.error {
            return Err(e);
        }
        Ok(DeviceIdentity {
            config: self.config,
            blob: &self.buf[..self.used],
        })
    }
}

// ---------------------------------------------------------------------------
// MSOS 2.0 descriptor set builder
//
// MSOS 2.0 descriptors are a tree:
//
//     Descriptor Set Header                       (device scope)
//       ├─ Feature descriptors                    (apply to whole device)
//       └─ Configuration Subset Header            (per bConfigurationValue)
//            ├─ Feature descriptors               (apply to config)
//            └─ Function Subset Header            (per first interface)
//                 └─ Feature descriptors          (apply to function)
//
// Each header carries a wTotalLength that covers the header itself plus all
// nested children. The builder tracks open scopes internally and backfills
// those length fields when their `with_*` closure returns.
//
// Typical usage:
//
// ```ignore
// let mut buf = [0u8; 256];
// let set = Msos20DescriptorSet::new(&mut buf)
//     .compatible_id("WINUSB", "")           // device-scope feature
//     .with_configuration(0, |cfg| {
//         cfg.with_function(1, |func| {
//             func.compatible_id("WINUSB", "")
//                 .registry_property(RegistryDataType::Sz, "Label", &utf16)
//         })
//     })
//     .build()
//     .unwrap();
// let plat_cap = msos_platform_capability(set.len() as u16, vendor_code);
// // then: identity.bos_capability(0x05, &plat_cap)
// //            .vendor_request(vendor_code, MSOS_DESCRIPTOR_INDEX, set)
// ```
//
// Sub-trees can be factored out by writing a function with the signature
// `fn(Msos20DescriptorSet) -> Msos20DescriptorSet` and threading it through
// the parent's closure — letting you build configurations or functions in
// isolation and combine them at the call site.
// ---------------------------------------------------------------------------

/// `dwWindowsVersion` for Windows 8.1 and later — the only value accepted
/// by the MSOS 2.0 parser in practice.
const MSOS_WINDOWS_VERSION: u32 = 0x0603_0000;

/// `wIndex` the host uses in the vendor control IN that retrieves the
/// MSOS 2.0 descriptor set (`MS_OS_20_DESCRIPTOR_INDEX`).
pub const MSOS_DESCRIPTOR_INDEX: u16 = 0x07;

// MSOS 2.0 descriptor type codes (wDescriptorType).
const MSOS20_SET_HEADER: u8 = 0x00;
const MSOS20_SUBSET_CONFIGURATION: u8 = 0x01;
const MSOS20_SUBSET_FUNCTION: u8 = 0x02;
const MSOS20_FEATURE_COMPATIBLE_ID: u8 = 0x03;
const MSOS20_FEATURE_REG_PROPERTY: u8 = 0x04;
const MSOS20_FEATURE_MIN_RESUME_TIME: u8 = 0x05;
const MSOS20_FEATURE_MODEL_ID: u8 = 0x06;
const MSOS20_FEATURE_CCGP_DEVICE: u8 = 0x07;
const MSOS20_FEATURE_VENDOR_REVISION: u8 = 0x08;

/// Windows registry property data types for `Msos20DescriptorSet::registry_property`.
#[repr(u16)]
#[derive(Clone, Copy)]
pub enum RegistryDataType {
    Sz = 1,
    ExpandSz = 2,
    Binary = 3,
    DwordLittleEndian = 4,
    DwordBigEndian = 5,
    Link = 6,
    MultiSz = 7,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, rcard_log::Format)]
pub enum MsosError {
    /// Scratch buffer ran out of room.
    BufferTooSmall,
    /// Internal scope-stack inconsistency. Should be unreachable from the
    /// public API but exists as a defensive check on the closure-based
    /// scope helpers.
    UnbalancedScopes,
    /// Tried to place a subset where the MSOS 2.0 spec doesn't allow it
    /// (e.g. a configuration subset inside a function, or a function inside
    /// another function).
    ScopeViolation,
    /// Tried to nest deeper than the MSOS 2.0 spec allows
    /// (Set → Configuration → Function).
    TooDeep,
    /// A single descriptor exceeded 65535 bytes.
    EntryTooLong,
}

/// A builder for an MSOS 2.0 descriptor set tree. Writes directly into a
/// caller-supplied scratch buffer and tracks open subset scopes so that
/// `wTotalLength` fields are filled in automatically on scope close.
///
/// The maximum nesting depth is 3 (root Set, Configuration subset, Function
/// subset) as required by the MSOS 2.0 specification.
pub struct Msos20DescriptorSet<'a> {
    buf: &'a mut [u8],
    used: usize,
    /// Byte offsets of the headers whose total-length fields still need
    /// backfilling. Index 0 is always the root set header.
    scope_offsets: [usize; 3],
    depth: usize,
    error: Option<MsosError>,
}

impl<'a> Msos20DescriptorSet<'a> {
    /// Start a new descriptor set in the given buffer. Emits the root
    /// Descriptor Set Header; `wTotalLength` is backfilled in [`build`].
    pub fn new(buf: &'a mut [u8]) -> Self {
        let mut s = Self {
            buf,
            used: 0,
            scope_offsets: [0; 3],
            depth: 0,
            error: None,
        };
        if s.buf.len() < 10 {
            s.error = Some(MsosError::BufferTooSmall);
            return s;
        }
        // Descriptor Set Header (10 bytes): wLength=10, wDescriptorType=0,
        // dwWindowsVersion, wTotalLength (filled later).
        s.buf[0] = 10;
        s.buf[1] = 0;
        s.buf[2] = MSOS20_SET_HEADER;
        s.buf[3] = 0;
        s.buf[4..8].copy_from_slice(&MSOS_WINDOWS_VERSION.to_le_bytes());
        s.buf[8] = 0;
        s.buf[9] = 0;
        s.scope_offsets[0] = 0;
        s.depth = 1;
        s.used = 10;
        s
    }

    fn reserve(&mut self, n: usize) -> Option<usize> {
        if self.error.is_some() {
            return None;
        }
        if self.used + n > self.buf.len() {
            self.error = Some(MsosError::BufferTooSmall);
            return None;
        }
        let off = self.used;
        self.used += n;
        Some(off)
    }

    fn write_feature_header(&mut self, off: usize, length: u16, feature_type: u8) {
        self.buf[off] = length as u8;
        self.buf[off + 1] = (length >> 8) as u8;
        self.buf[off + 2] = feature_type;
        self.buf[off + 3] = 0;
    }

    /// Add a CompatibleID feature descriptor at the current scope.
    /// Both `id` and `sub_id` are truncated/zero-padded to 8 bytes.
    pub fn compatible_id(mut self, id: &str, sub_id: &str) -> Self {
        let Some(off) = self.reserve(20) else {
            return self;
        };
        self.write_feature_header(off, 20, MSOS20_FEATURE_COMPATIBLE_ID);
        for b in &mut self.buf[off + 4..off + 20] {
            *b = 0;
        }
        let idb = id.as_bytes();
        let n = idb.len().min(8);
        self.buf[off + 4..off + 4 + n].copy_from_slice(&idb[..n]);
        let sb = sub_id.as_bytes();
        let m = sb.len().min(8);
        self.buf[off + 12..off + 12 + m].copy_from_slice(&sb[..m]);
        self
    }

    /// Add a MinResumeTime feature descriptor. Device scope only (per spec).
    pub fn min_resume_time(mut self, resume_recovery_ms: u8, resume_signaling_ms: u8) -> Self {
        let Some(off) = self.reserve(6) else {
            return self;
        };
        self.write_feature_header(off, 6, MSOS20_FEATURE_MIN_RESUME_TIME);
        self.buf[off + 4] = resume_recovery_ms;
        self.buf[off + 5] = resume_signaling_ms;
        self
    }

    /// Add a ModelID feature descriptor. Device scope only (per spec).
    pub fn model_id(mut self, model_id: [u8; 16]) -> Self {
        let Some(off) = self.reserve(20) else {
            return self;
        };
        self.write_feature_header(off, 20, MSOS20_FEATURE_MODEL_ID);
        self.buf[off + 4..off + 20].copy_from_slice(&model_id);
        self
    }

    /// Add a CCGP Device feature descriptor — marks the device as a
    /// Microsoft-provided USB composite (OS picks the class driver).
    pub fn ccgp_device(mut self) -> Self {
        let Some(off) = self.reserve(4) else {
            return self;
        };
        self.write_feature_header(off, 4, MSOS20_FEATURE_CCGP_DEVICE);
        self
    }

    /// Add a VendorRevision feature descriptor.
    pub fn vendor_revision(mut self, revision: u16) -> Self {
        let Some(off) = self.reserve(6) else {
            return self;
        };
        self.write_feature_header(off, 6, MSOS20_FEATURE_VENDOR_REVISION);
        self.buf[off + 4] = revision as u8;
        self.buf[off + 5] = (revision >> 8) as u8;
        self
    }

    /// Add a Registry Property feature descriptor.
    ///
    /// - `name` is encoded to UTF-16LE with a trailing NUL (ASCII only).
    /// - `data` is the raw property value. For REG_SZ / REG_MULTI_SZ /
    ///   REG_EXPAND_SZ, callers are responsible for providing UTF-16LE
    ///   NUL-terminated data.
    pub fn registry_property(
        mut self,
        data_type: RegistryDataType,
        name: &str,
        data: &[u8],
    ) -> Self {
        // Layout: wLength(2), wType(2), wPropDataType(2), wPropNameLen(2),
        // PropName(UTF-16LE, NUL-terminated), wPropDataLen(2), PropData.
        let name_bytes_len = (name.len() + 1) * 2;
        let total = 8 + name_bytes_len + 2 + data.len();
        if total > u16::MAX as usize {
            self.error = Some(MsosError::EntryTooLong);
            return self;
        }
        let Some(off) = self.reserve(total) else {
            return self;
        };
        let total_u16 = total as u16;
        self.write_feature_header(off, total_u16, MSOS20_FEATURE_REG_PROPERTY);
        let dt = data_type as u16;
        self.buf[off + 4] = dt as u8;
        self.buf[off + 5] = (dt >> 8) as u8;
        let nbl = name_bytes_len as u16;
        self.buf[off + 6] = nbl as u8;
        self.buf[off + 7] = (nbl >> 8) as u8;
        let mut p = off + 8;
        for b in name.bytes() {
            self.buf[p] = b;
            self.buf[p + 1] = 0;
            p += 2;
        }
        self.buf[p] = 0;
        self.buf[p + 1] = 0;
        p += 2;
        let dl = data.len() as u16;
        self.buf[p] = dl as u8;
        self.buf[p + 1] = (dl >> 8) as u8;
        p += 2;
        self.buf[p..p + data.len()].copy_from_slice(data);
        self
    }

    /// Add a Configuration Subset to the tree. Features and functions added
    /// inside the closure apply to the configuration with
    /// `bConfigurationValue = value`. The configuration subset's
    /// `wTotalLength` is filled in automatically when the closure returns.
    ///
    /// Configuration subsets may only appear directly under the root
    /// descriptor set; nesting one inside a function or another configuration
    /// is a `MsosError::ScopeViolation`.
    pub fn with_configuration<F>(mut self, value: u8, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if self.error.is_none() && self.depth != 1 {
            self.error = Some(MsosError::ScopeViolation);
        }
        self = self.push_subset(MSOS20_SUBSET_CONFIGURATION, value);
        let mut s = f(self);
        s.pop_subset(MSOS20_SUBSET_CONFIGURATION);
        s
    }

    /// Add a Function Subset to the tree. Features added inside the closure
    /// apply to the function whose first interface is `first_interface`.
    /// The subset's `wSubsetLength` is filled in automatically when the
    /// closure returns.
    ///
    /// Function subsets may appear directly under the root set (single-
    /// configuration devices) or inside a Configuration Subset; nesting one
    /// inside another function is a `MsosError::ScopeViolation`.
    pub fn with_function<F>(mut self, first_interface: u8, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if self.error.is_none() && self.depth >= 3 {
            // Already inside a function — would imply function-in-function.
            self.error = Some(MsosError::ScopeViolation);
        }
        // Reject function-in-function explicitly even if depth check above
        // didn't fire (e.g. if we somehow extended scope_offsets later).
        if self.error.is_none()
            && self.depth >= 2
            && self.buf[self.scope_offsets[self.depth - 1] + 2] == MSOS20_SUBSET_FUNCTION
        {
            self.error = Some(MsosError::ScopeViolation);
        }
        self = self.push_subset(MSOS20_SUBSET_FUNCTION, first_interface);
        let mut s = f(self);
        s.pop_subset(MSOS20_SUBSET_FUNCTION);
        s
    }

    fn push_subset(mut self, subset_type: u8, header_byte_4: u8) -> Self {
        if self.error.is_some() {
            return self;
        }
        if self.depth >= self.scope_offsets.len() {
            self.error = Some(MsosError::TooDeep);
            return self;
        }
        let Some(off) = self.reserve(8) else {
            return self;
        };
        self.buf[off] = 8;
        self.buf[off + 1] = 0;
        self.buf[off + 2] = subset_type;
        self.buf[off + 3] = 0;
        self.buf[off + 4] = header_byte_4;
        self.buf[off + 5] = 0;
        self.buf[off + 6] = 0;
        self.buf[off + 7] = 0;
        self.scope_offsets[self.depth] = off;
        self.depth += 1;
        self
    }

    fn pop_subset(&mut self, expected_type: u8) {
        if self.error.is_some() {
            return;
        }
        if self.depth < 2 {
            self.error = Some(MsosError::UnbalancedScopes);
            return;
        }
        let start = self.scope_offsets[self.depth - 1];
        if self.buf[start + 2] != expected_type {
            self.error = Some(MsosError::UnbalancedScopes);
            return;
        }
        let total = (self.used - start) as u16;
        self.buf[start + 6] = total as u8;
        self.buf[start + 7] = (total >> 8) as u8;
        self.depth -= 1;
    }

    /// Finalize the descriptor set. Backfills the root `wTotalLength` and
    /// returns a byte slice of the complete MSOS 2.0 tree, ready to be
    /// shipped as a vendor control response.
    pub fn build(self) -> Result<&'a [u8], MsosError> {
        if let Some(e) = self.error {
            return Err(e);
        }
        if self.depth != 1 {
            return Err(MsosError::UnbalancedScopes);
        }
        if self.used > u16::MAX as usize {
            return Err(MsosError::EntryTooLong);
        }
        let total = self.used as u16;
        self.buf[8] = total as u8;
        self.buf[9] = (total >> 8) as u8;
        Ok(&self.buf[..self.used])
    }
}

/// Build the payload of an MSOS 2.0 BOS Platform Capability descriptor —
/// the 25 bytes that follow `bDevCapabilityType` when the capability is
/// emitted inside a BOS.
///
/// The returned payload references a descriptor set of `descriptor_set_len`
/// bytes retrieved via a vendor control IN with `bRequest = vendor_code`
/// and `wIndex = MSOS_DESCRIPTOR_INDEX`.
///
/// Structure:
///   bReserved (1)
///   PlatformCapabilityUUID (16) — D8DD60DF-4589-4CC7-9CD2-659D9E648A9F
///   dwWindowsVersion (4)       — 0x06030000 (Windows 8.1+)
///   wMSOSDescriptorSetTotalLength (2)
///   bMS_VendorCode (1)
///   bAltEnumCode (1)
pub fn msos_platform_capability(descriptor_set_len: u16, vendor_code: u8) -> [u8; 25] {
    let mut pc = [0u8; 25];
    // bReserved = 0 at pc[0]
    // PlatformCapabilityUUID — mixed-endian per the MS OS 2.0 spec
    pc[1] = 0xDF;
    pc[2] = 0x60;
    pc[3] = 0xDD;
    pc[4] = 0xD8;
    pc[5] = 0x89;
    pc[6] = 0x45;
    pc[7] = 0xC7;
    pc[8] = 0x4C;
    pc[9] = 0x9C;
    pc[10] = 0xD2;
    pc[11] = 0x65;
    pc[12] = 0x9D;
    pc[13] = 0x9E;
    pc[14] = 0x64;
    pc[15] = 0x8A;
    pc[16] = 0x9F;
    pc[17..21].copy_from_slice(&MSOS_WINDOWS_VERSION.to_le_bytes());
    pc[21] = descriptor_set_len as u8;
    pc[22] = (descriptor_set_len >> 8) as u8;
    pc[23] = vendor_code;
    // bAltEnumCode = 0
    pc
}

// ---------------------------------------------------------------------------
// Blob parser — used by the server to read a lease-delivered identity blob.
// ---------------------------------------------------------------------------

/// A view over a TLV identity blob, with typed accessors for known entries.
#[derive(Clone, Copy)]
pub struct DeviceBlob<'a>(pub &'a [u8]);

impl<'a> DeviceBlob<'a> {
    pub fn iter(&self) -> TlvIter<'a> {
        TlvIter {
            blob: self.0,
            pos: 0,
        }
    }

    /// Walk the blob once, checking that every entry is well-formed.
    pub fn validate(&self) -> bool {
        let mut iter = self.iter();
        while iter.next().is_some() {}
        iter.pos == self.0.len()
    }

    fn find(&self, tag: u8) -> Option<&'a [u8]> {
        self.iter().find(|(t, _)| *t == tag).map(|(_, d)| d)
    }

    pub fn manufacturer(&self) -> Option<&'a str> {
        self.find(TAG_MANUFACTURER)
            .and_then(|d| core::str::from_utf8(d).ok())
    }

    pub fn product(&self) -> Option<&'a str> {
        self.find(TAG_PRODUCT)
            .and_then(|d| core::str::from_utf8(d).ok())
    }

    pub fn serial(&self) -> Option<&'a str> {
        self.find(TAG_SERIAL)
            .and_then(|d| core::str::from_utf8(d).ok())
    }

    /// Iterate BOS device capability descriptors. Yields `(cap_type, payload)`.
    pub fn bos_capabilities(&self) -> impl Iterator<Item = (u8, &'a [u8])> {
        self.iter().filter_map(|(tag, data)| {
            if tag == TAG_BOS_CAPABILITY && !data.is_empty() {
                Some((data[0], &data[1..]))
            } else {
                None
            }
        })
    }

    /// Look up a vendor request handler by `(bRequest, wIndex)`.
    pub fn vendor_request(&self, code: u8, index: u16) -> Option<&'a [u8]> {
        self.iter().find_map(|(tag, data)| {
            if tag != TAG_VENDOR_REQUEST || data.len() < 3 || data[0] != code {
                return None;
            }
            let idx = u16::from_le_bytes([data[1], data[2]]);
            if idx == index {
                Some(&data[3..])
            } else {
                None
            }
        })
    }

    /// True if the blob contains any BOS capability entries. Devices with
    /// BOS descriptors must advertise USB 2.1.
    pub fn has_bos(&self) -> bool {
        self.bos_capabilities().next().is_some()
    }
}

pub struct TlvIter<'a> {
    blob: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for TlvIter<'a> {
    type Item = (u8, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos + 3 > self.blob.len() {
            return None;
        }
        let tag = self.blob[self.pos];
        let len =
            u16::from_le_bytes([self.blob[self.pos + 1], self.blob[self.pos + 2]]) as usize;
        let data_start = self.pos + 3;
        let data_end = data_start + len;
        if data_end > self.blob.len() {
            return None;
        }
        let data = &self.blob[data_start..data_end];
        self.pos = data_end;
        Some((tag, data))
    }
}

// ---------------------------------------------------------------------------
// Endpoint types
// ---------------------------------------------------------------------------

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
)]
#[repr(C, packed)]
pub struct EndpointConfig {
    /// Endpoint number (1–15). EP0 is managed internally.
    pub number: u8,
    pub direction: Direction,
    pub transfer_type: TransferType,
    pub max_packet_size: u16,
    /// Polling interval for interrupt/isochronous (ignored for bulk).
    pub interval: u8,
}

/// Opaque handle returned by `UsbBus::take_endpoint_handle`.
/// Consumed by `UsbEndpoint::open` to bind an endpoint to the current
/// bus session.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    zerocopy::FromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
)]
#[repr(transparent)]
pub struct EndpointHandle(pub u32);

// ---------------------------------------------------------------------------
// Resource traits
// ---------------------------------------------------------------------------

/// The USB bus — singleton representing the USB peripheral.
///
/// Lifecycle:
///   1. `take()` claims the bus with a fixed config and an identity blob.
///   2. `take_endpoint_handle()` hands out handles (up to `endpoints`).
///   3. Each handle is consumed by `UsbEndpoint::open()`.
///   4. Once all endpoints are open, the bus auto-enables (attaches to host).
///   5. Dropping `UsbBus` detaches and tears down all endpoints.
#[ipc::resource(arena_size = 1, kind = 0x30)]
pub trait UsbBus {
    /// Claim the USB peripheral. `config` carries the fixed device fields;
    /// `blob` is a TLV stream of strings, BOS capabilities, and vendor
    /// request handlers (see `DeviceIdentity::builder`). `endpoints` declares
    /// how many endpoints will be configured before the bus attaches.
    #[constructor]
    fn take(
        config: FixedDeviceConfig,
        #[lease] blob: &[u8],
        endpoints: u8,
    ) -> Result<Self, UsbError>;

    /// Current bus state.
    #[message]
    fn state(&self) -> BusState;

    /// Obtain a handle for opening one endpoint. Returns `None` when all
    /// handles for this session have been claimed.
    #[message]
    fn take_endpoint_handle(&self) -> Option<EndpointHandle>;
}

/// A configured USB endpoint for data transfer.
#[ipc::resource(arena_size = 8, kind = 0x31)]
pub trait UsbEndpoint {
    /// Open an endpoint using a handle obtained from `UsbBus::take_endpoint_handle`.
    #[constructor]
    fn open(handle: EndpointHandle, config: EndpointConfig) -> Result<Self, UsbError>;

    /// Write to an IN endpoint (device → host). Returns bytes written.
    #[message]
    fn write(&mut self, #[lease] data: &[u8]) -> Result<u16, UsbError>;

    /// Read from an OUT endpoint (host → device). Returns bytes read.
    #[message]
    fn read(&mut self, #[lease] buf: &mut [u8]) -> Result<u16, UsbError>;

    /// Set or clear the STALL condition on this endpoint.
    #[message]
    fn set_stall(&mut self, stalled: bool);
}
