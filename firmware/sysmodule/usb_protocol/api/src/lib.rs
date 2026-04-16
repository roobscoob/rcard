#![no_std]

pub use sysmodule_usb_api::{BusState, EndpointHandle};

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
)]
#[repr(u8)]
pub enum ManagerError {
    /// The requested handles have already been taken by another task.
    AlreadyTaken = 0,
    /// The USB bus is not yet configured.
    NotReady = 1,
}

/// Packed pair of endpoint handles for one channel.
#[derive(
    Clone,
    Copy,
    Debug,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
    zerocopy::FromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
    rcard_log::Format,
)]
#[repr(C, packed)]
pub struct HandlePair {
    pub ep_out: EndpointHandle,
    pub ep_in: EndpointHandle,
}

#[ipc::resource(arena_size = 0, kind = 0x32)]
pub trait UsbProtocolManager {
    /// Take the endpoint handles for the host-driven channel.
    /// Returns (OUT, IN). Can only be called once.
    #[message]
    fn take_host_handles() -> Result<HandlePair, ManagerError>;

    /// Take the endpoint handles for the fob-driven channel.
    /// Returns (OUT, IN). Can only be called once.
    #[message]
    fn take_fob_handles() -> Result<HandlePair, ManagerError>;

    /// Current USB bus state.
    #[message]
    fn bus_state() -> BusState;
}
