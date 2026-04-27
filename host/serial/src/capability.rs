use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use crate::sifli_debug::{DebugHandle, Error};

/// Capability: SifliDebug interface available on this device.
///
/// Provided by the Usart1 adapter. Use `try_acquire()` to enter debug mode
/// and get a `DebugSession` guard with mem_read/mem_write access.
#[derive(Clone)]
pub struct SifliDebug {
    handle: Arc<DebugHandle>,
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl SifliDebug {
    pub(crate) fn new(handle: Arc<DebugHandle>) -> Self {
        SifliDebug {
            handle,
            lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Put the underlying tap into sentinel-resync mode.
    ///
    /// Forwards every byte as passthrough noise until the sentinel is found
    /// on the wire, then returns the tap to normal framing. Returns
    /// `Error::Timeout` if `timeout` elapses before the sentinel arrives;
    /// the tap is left in resync mode in that case and will recover on its
    /// own when the sentinel finally appears. Use after any command that
    /// may cut the wire mid-frame (e.g. a soft reset).
    pub async fn resync_on_sentinel(
        &self,
        sentinel: Vec<u8>,
        timeout: Duration,
    ) -> Result<(), Error> {
        self.handle.resync_on_sentinel(sentinel, timeout).await
    }

    /// Poison the debug handle: any in-flight request (including a
    /// pending Exit from a dropped session) returns immediately.
    /// Call when the device has rebooted and existing sessions are dead.
    pub fn poison(&self) {
        self.handle.poison();
    }

    /// Try to enter debug mode. Returns a session guard if the device
    /// responds within 1 second, or `None` on timeout.
    pub async fn try_acquire(&self) -> Option<DebugSession> {
        eprintln!("[sifli] try_acquire: waiting for lock...");
        let guard = self.lock.clone().lock_owned().await;
        eprintln!("[sifli] try_acquire: lock acquired, entering debug...");
        match tokio::time::timeout(Duration::from_secs(1), self.handle.enter()).await {
            Ok(Ok(poison_gen)) => Some(DebugSession {
                handle: self.handle.clone(),
                guard: Some(guard),
                exit_on_drop: true,
                poison_gen,
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
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    exit_on_drop: bool,
    /// Poison generation at the time `enter()` succeeded. If the handle's
    /// generation has moved past this value, the device rebooted and all
    /// requests (including the Drop-spawned Exit) fail immediately.
    poison_gen: u64,
}

impl DebugSession {
    /// Read `count` 32-bit words starting at `addr`.
    pub async fn mem_read(&self, addr: u32, count: u16) -> Result<Vec<u32>, Error> {
        tokio::time::timeout(
            Duration::from_secs(1),
            self.handle.mem_read(self.poison_gen, addr, count),
        )
        .await
        .map_err(|_| Error::Timeout)?
    }

    /// Write 32-bit words to `addr`.
    ///
    /// The timeout scales with payload size: 2 s base plus 1 s per ~50 KB
    /// (half of the 1 Mbaud wire speed, conservatively). This lets a
    /// multi-MB chunk complete without the fixed 1 s ceiling that applies
    /// to other commands.
    pub async fn mem_write(&self, addr: u32, data: &[u32]) -> Result<(), Error> {
        let payload_bytes = (data.len() * 4) as u64;
        let timeout = Duration::from_secs(2) + Duration::from_millis(payload_bytes / 50);
        tokio::time::timeout(timeout, self.handle.mem_write(self.poison_gen, addr, data))
            .await
            .map_err(|_| Error::Timeout)?
    }

    /// Write 32-bit words without waiting for a response.
    /// Use for operations that kill the connection (e.g. soft reset).
    pub async fn mem_write_and_forget(mut self, addr: u32, data: &[u32]) -> Result<(), Error> {
        self.exit_on_drop = false;
        self.handle.mem_write_no_response(addr, data).await
    }

    /// Shared atomic counter of bytes written to the underlying writer.
    ///
    /// Use to drive a fine-grained progress bar: sample while a long
    /// `mem_write` is in flight.
    pub fn byte_counter(&self) -> Arc<AtomicU64> {
        self.handle.byte_counter()
    }

    /// Abandon this session without sending an Exit command.
    ///
    /// Use when the device has rebooted or the connection is known to be
    /// dead — sending Exit would just timeout against noise.
    pub fn forget(mut self) {
        self.exit_on_drop = false;
    }
}

impl Drop for DebugSession {
    fn drop(&mut self) {
        let guard = self.guard.take();
        if !self.exit_on_drop {
            return;
        }
        let handle = self.handle.clone();
        let poison_gen = self.poison_gen;
        tokio::spawn(async move {
            eprintln!("[sifli] drop: sending Exit...");
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                handle.exit(poison_gen),
            )
            .await;
            eprintln!("[sifli] drop: Exit returned {result:?}, releasing lock");
            drop(guard);
        });
    }
}
