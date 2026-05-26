//! bt-hci async Transport over the sysmodule_lcpu IPC interface.

use bt_hci::{ControllerToHostPacket, FromHciBytes, FromHciBytesError, HostToControllerPacket, WriteHci};
use bt_hci::transport::WithIndicator;
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

    async fn write<T: HostToControllerPacket>(
        &self,
        val: &T,
    ) -> Result<(), Self::Error> {
        // H4-frame the packet: prepend the PacketKind indicator byte so the
        // LCPU controller knows whether to interpret the payload as Cmd /
        // ACL / SCO / Event / ISO. Without this prefix, even a well-formed
        // HCI_Reset is silently dropped by the controller and no Command
        // Complete event comes back, hanging trouble's runner.
        let mut buf = [0u8; 256];
        let wrapped = WithIndicator::new(val);
        let n = wrapped.size();
        if n > buf.len() {
            return Err(HciError::WriteTooLarge);
        }
        wrapped
            .write_hci(&mut buf[..n])
            .map_err(|_| HciError::WriteTooLarge)?;
        crate::lcpu_send_hci(&buf[..n]).map_err(|_| HciError::SendFailed)?;
        Ok(())
    }
}
