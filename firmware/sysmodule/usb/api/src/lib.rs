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
