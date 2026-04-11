use std::sync::Arc;
use std::time::Duration;

use crate::sifli_debug::{DebugHandle, Error};

/// Capability: SifliDebug interface available on this device.
///
/// Provided by the Usart1 adapter. Use `try_acquire()` to enter debug mode
/// and get a `DebugSession` guard with mem_read/mem_write access.
pub struct SifliDebug {
    handle: Arc<DebugHandle>,
}

impl SifliDebug {
    pub(crate) fn new(handle: Arc<DebugHandle>) -> Self {
        SifliDebug { handle }
    }

    /// Put the underlying tap into sentinel-resync mode.
    ///
    /// Forwards every byte as passthrough noise until the sentinel is found
    /// on the wire, then returns the tap to normal framing. Use after any
    /// command that may cut the wire mid-frame (e.g. a soft reset).
    pub async fn resync_on_sentinel(&self, sentinel: Vec<u8>) -> Result<(), Error> {
        self.handle.resync_on_sentinel(sentinel).await
    }

    /// Try to enter debug mode. Returns a session guard if the device
    /// responds within 1 second, or `None` on timeout.
    pub async fn try_acquire(&self) -> Option<DebugSession> {
        match tokio::time::timeout(Duration::from_secs(1), self.handle.enter()).await {
            Ok(Ok(())) => Some(DebugSession {
                handle: self.handle.clone(),
                exit_on_drop: true,
            }),
            _ => None,
        }
    }
}

/// A live debug session. Provides mem_read/mem_write access.
///
/// Automatically exits debug mode when dropped, unless `forget()` is called
/// first (e.g. after a soft reset that already killed the connection).
pub struct DebugSession {
    handle: Arc<DebugHandle>,
    exit_on_drop: bool,
}

impl DebugSession {
    /// Read `count` 32-bit words starting at `addr`.
    pub async fn mem_read(&self, addr: u32, count: u16) -> Result<Vec<u32>, Error> {
        tokio::time::timeout(Duration::from_secs(1), self.handle.mem_read(addr, count))
            .await
            .map_err(|_| Error::Timeout)?
    }

    /// Write 32-bit words to `addr`.
    pub async fn mem_write(&self, addr: u32, data: &[u32]) -> Result<(), Error> {
        tokio::time::timeout(Duration::from_secs(1), self.handle.mem_write(addr, data))
            .await
            .map_err(|_| Error::Timeout)?
    }

    /// Write 32-bit words without waiting for a response.
    /// Use for operations that kill the connection (e.g. soft reset).
    pub async fn mem_write_no_response(&self, addr: u32, data: &[u32]) -> Result<(), Error> {
        self.handle.mem_write_no_response(addr, data).await
    }

    /// Consume the session without sending `Exit`.
    ///
    /// For use after a reset write: the chip is already rebooting and cannot
    /// answer an Exit, so skip the `Drop`-spawned exit entirely.
    pub fn forget(mut self) {
        self.exit_on_drop = false;
    }
}

impl Drop for DebugSession {
    fn drop(&mut self) {
        if !self.exit_on_drop {
            return;
        }
        let handle = self.handle.clone();
        tokio::spawn(async move {
            let _ = handle.exit().await;
        });
    }
}
