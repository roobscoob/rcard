#![no_std]

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

#[derive(
    Copy, Clone, Debug, PartialEq, Eq,
    Serialize, Deserialize, Schema,
    rcard_log::Format,
)]
#[repr(u8)]
pub enum BtError {
    LcpuInitFailed = 0,
    NotConnected = 1,
    NotAdvertising = 2,
    AlreadyAdvertising = 3,
    NotifFailed = 4,
    Busy = 5,
    HostInitFailed = 6,
    AdvertiseFailed = 7,
    WriteFailed = 8,
}

#[derive(
    Copy, Clone, Debug, PartialEq, Eq,
    Serialize, Deserialize, Schema,
    rcard_log::Format,
    zerocopy::TryFromBytes, zerocopy::IntoBytes,
    zerocopy::KnownLayout, zerocopy::Immutable,
)]
#[repr(u8)]
pub enum ConnectionState {
    Disconnected = 0,
    Advertising = 1,
    Connected = 2,
}

#[derive(
    Copy, Clone, Debug,
    Serialize, Deserialize, Schema,
    zerocopy::TryFromBytes, zerocopy::IntoBytes,
    zerocopy::KnownLayout, zerocopy::Immutable,
)]
#[repr(C)]
pub struct CharValue {
    pub data: [u8; 20],
    pub len: u8,
}

impl CharValue {
    pub fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

#[ipc::resource(arena_size = 1, kind = 0x40)]
pub trait Bluetooth {
    /// Initialize the BLE stack. Brings up the LCPU, creates the trouble
    /// Host, and prepares for advertising. `device_name` is null-padded;
    /// `name_len` is the actual length.
    #[constructor]
    fn init(
        bd_addr: [u8; 6],
        device_name: [u8; 32],
        name_len: u8,
    ) -> Result<Self, BtError>;

    /// Begin advertising with the device name from init. If already
    /// advertising, returns `AlreadyAdvertising`.
    #[message]
    fn start_advertising(&mut self) -> Result<(), BtError>;

    /// Stop advertising. No-op if not advertising.
    #[message]
    fn stop_advertising(&mut self) -> Result<(), BtError>;

    /// Update the custom characteristic's local value. Does not send a
    /// notification — call `notify` for that.
    #[message]
    fn write_characteristic(&mut self, data: [u8; 20], len: u8) -> Result<(), BtError>;

    /// Read the current local value of the custom characteristic.
    #[message]
    fn read_characteristic(&mut self) -> CharValue;

    /// Push a notification of the custom characteristic to the connected
    /// peer. Returns `NotConnected` if no peer is connected, or
    /// `NotifFailed` if the peer hasn't enabled notifications.
    #[message]
    fn notify(&mut self, data: [u8; 20], len: u8) -> Result<(), BtError>;

    /// Current connection state.
    #[message]
    fn connection_state(&mut self) -> ConnectionState;
}
