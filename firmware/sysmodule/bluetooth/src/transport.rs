//! bt-hci async Transport over the sysmodule_lcpu IPC interface.

use bt_hci::{ControllerToHostPacket, FromHciBytes, FromHciBytesError, HostToControllerPacket};
use ipc::executor::Signal;

pub static HCI_RX_SIGNAL: Signal<()> = Signal::new();

pub struct HciTransport;

#[derive(Debug)]
#[allow(dead_code)]
pub enum HciError {
    SendFailed,
    WriteTooLarge,
    Parse(FromHciBytesError),
}

impl core::fmt::Display for HciError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

impl core::error::Error for HciError {}

impl From<FromHciBytesError> for HciError {
    fn from(e: FromHciBytesError) -> Self {
        HciError::Parse(e)
    }
}

impl embedded_io::Error for HciError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

impl embedded_io::ErrorType for HciTransport {
    type Error = HciError;
}

impl bt_hci::transport::Transport for HciTransport {
    async fn read<'a>(
        &self,
        rx: &'a mut [u8],
    ) -> Result<ControllerToHostPacket<'a>, Self::Error> {
        loop {
            let n = crate::lcpu_recv_hci(rx);
            if n > 0 {
                let (packet, _) = ControllerToHostPacket::from_hci_bytes(&rx[..n])?;
                return Ok(packet);
            }
            HCI_RX_SIGNAL.wait().await;
        }
    }

    async fn write<T: HostToControllerPacket + ?Sized>(
        &self,
        val: &T,
    ) -> Result<(), Self::Error> {
        let mut buf = [0u8; 256];
        let n = val.size();
        if n > buf.len() {
            return Err(HciError::WriteTooLarge);
        }
        val.write_hci(&mut buf[..n]).map_err(|_| HciError::WriteTooLarge)?;
        crate::lcpu_send_hci(&buf[..n]).map_err(|_| HciError::SendFailed)?;
        Ok(())
    }
}
