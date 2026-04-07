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
pub struct DeviceIdentity {
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub bcd_device: u16,
    pub manufacturer: [u8; 32],
    pub product: [u8; 32],
    pub serial: [u8; 32],
    /// Pre-built MSOS 2.0 BOS platform capability payload (25 bytes).
    /// All zeros = no MSOS support. Built by `windows_driver()`.
    pub msos_platform_capability: [u8; 25],
    /// Pre-built MSOS 2.0 descriptor set (30 bytes).
    /// Served in response to the MSOS vendor request. Built by `windows_driver()`.
    pub msos_descriptor_set: [u8; 30],
    /// MSOS 2.0 vendor request code. Only meaningful when MSOS is enabled.
    pub msos_vendor_code: u8,
}

impl DeviceIdentity {
    /// Create a new identity with the given VID/PID and no string descriptors.
    pub fn new(vendor_id: u16, product_id: u16) -> Self {
        Self {
            vendor_id,
            product_id,
            device_class: 0,
            device_subclass: 0,
            device_protocol: 0,
            bcd_device: 0x0100,
            manufacturer: [0; 32],
            product: [0; 32],
            serial: [0; 32],
            msos_platform_capability: [0; 25],
            msos_descriptor_set: [0; 30],
            msos_vendor_code: 0,
        }
    }

    pub fn device_class(mut self, class: u8, subclass: u8, protocol: u8) -> Self {
        self.device_class = class;
        self.device_subclass = subclass;
        self.device_protocol = protocol;
        self
    }

    pub fn device_release(mut self, bcd: u16) -> Self {
        self.bcd_device = bcd;
        self
    }

    pub fn manufacturer(mut self, s: &str) -> Self {
        let bytes = s.as_bytes();
        let len = bytes.len().min(32);
        self.manufacturer[..len].copy_from_slice(&bytes[..len]);
        self
    }

    pub fn product(mut self, s: &str) -> Self {
        let bytes = s.as_bytes();
        let len = bytes.len().min(32);
        self.product[..len].copy_from_slice(&bytes[..len]);
        self
    }

    pub fn serial_number(mut self, s: &str) -> Self {
        let bytes = s.as_bytes();
        let len = bytes.len().min(32);
        self.serial[..len].copy_from_slice(&bytes[..len]);
        self
    }

    /// Enable automatic Windows driver binding via MSOS 2.0 descriptors.
    /// Common compatible IDs: "WINUSB", "LIBUSB0", "LIBUSBK".
    pub fn windows_driver(self, compatible_id: &str) -> Self {
        self.windows_driver_full(compatible_id, "", 0x01)
    }

    /// Enable MSOS 2.0 with full control over compatible ID, sub-compatible ID,
    /// and vendor request code.
    pub fn windows_driver_full(
        mut self,
        compatible_id: &str,
        sub_compatible_id: &str,
        vendor_code: u8,
    ) -> Self {
        self.msos_vendor_code = vendor_code;

        // Build the 30-byte descriptor set
        let mut ds = [0u8; 30];
        // Descriptor Set Header (10 bytes)
        ds[0] = 10; // wLength
                    // wDescriptorType = MS_OS_20_SET_HEADER = 0x0000
                    // dwWindowsVersion = 0x06030000 (Windows 8.1+)
        ds[4] = 0x00;
        ds[5] = 0x00;
        ds[6] = 0x03;
        ds[7] = 0x06;
        ds[8] = 30; // wTotalLength
                    // Compatible ID Descriptor (20 bytes)
        ds[10] = 20; // wLength
        ds[12] = 0x03; // wDescriptorType = MS_OS_20_FEATURE_COMPATIBLE_ID
                       // CompatibleID (8 bytes)
        let cid = compatible_id.as_bytes();
        let clen = cid.len().min(8);
        ds[14..14 + clen].copy_from_slice(&cid[..clen]);
        // SubCompatibleID (8 bytes)
        let sid = sub_compatible_id.as_bytes();
        let slen = sid.len().min(8);
        ds[22..22 + slen].copy_from_slice(&sid[..slen]);
        self.msos_descriptor_set = ds;

        // Build the 25-byte BOS platform capability payload
        // UUID: {D8DD60DF-4589-4CC7-9CD2-659D9E648A9F} mixed-endian
        let mut pc = [0u8; 25];
        // bReserved = 0
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
        // dwWindowsVersion
        pc[17] = 0x00;
        pc[18] = 0x00;
        pc[19] = 0x03;
        pc[20] = 0x06;
        // wMSOSDescriptorSetTotalLength = 30
        pc[21] = 30;
        // bMS_VendorCode
        pc[23] = vendor_code;
        // bAltEnumCode = 0
        self.msos_platform_capability = pc;

        self
    }

    /// Whether MSOS 2.0 descriptors are configured.
    pub fn has_msos(&self) -> bool {
        self.msos_platform_capability[1] != 0
    }

    /// Get manufacturer as &str (up to first null byte).
    pub fn manufacturer_str(&self) -> &str {
        let len = self.manufacturer.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.manufacturer[..len]).unwrap_or("")
    }

    /// Get product as &str (up to first null byte).
    pub fn product_str(&self) -> &str {
        let len = self.product.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.product[..len]).unwrap_or("")
    }

    /// Get serial as &str (up to first null byte).
    pub fn serial_str(&self) -> &str {
        let len = self.serial.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.serial[..len]).unwrap_or("")
    }
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
///   1. `take()` claims the bus with a device identity and endpoint count.
///   2. `take_endpoint_handle()` hands out handles (up to the declared count).
///   3. Each handle is consumed by `UsbEndpoint::open()`.
///   4. Once all endpoints are open, the bus auto-enables (attaches to host).
///   5. Dropping `UsbBus` detaches and tears down all endpoints.
#[ipc::resource(arena_size = 1, kind = 0x30)]
pub trait UsbBus {
    /// Claim the USB peripheral. `endpoints` declares how many endpoints
    /// will be configured before the bus attaches.
    #[constructor]
    fn take(identity: DeviceIdentity, endpoints: u8) -> Result<Self, UsbError>;

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
