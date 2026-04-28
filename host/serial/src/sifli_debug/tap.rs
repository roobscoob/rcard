use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::sync::{mpsc, watch};

use super::frame::Frame;
use super::protocol::{Command, Error, Response};

/// Writer adapter that counts bytes handed to `poll_write`.
///
/// Used to drive a fine-grained flash progress bar: the observer task
/// samples the counter while a long `mem_write` is in flight.
struct ProgressWriter<W> {
    inner: W,
    counter: Arc<AtomicU64>,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for ProgressWriter<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &result {
            this.counter.fetch_add(*n as u64, Ordering::Relaxed);
        }
        result
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// Control messages to the background read loop.
enum TapControl {
    /// Switch the read loop into sentinel-resync mode: forward every byte as
    /// passthrough noise while matching against `sentinel`. On match, drop
    /// any parser state, signal `done`, and resume normal framing.
    ResyncOnSentinel {
        sentinel: Vec<u8>,
        done: tokio::sync::oneshot::Sender<()>,
    },
}

/// Split raw serial IO into a debug handle and transparent passthrough IO.
///
/// Spawns a background task that reads from `reader`, routing SifliDebug
/// frames to the `DebugHandle` and forwarding everything else through
/// `TapReader`. Both `TapWriter` and `DebugHandle` share the writer.
pub fn tap<R, W>(reader: R, writer: W) -> (DebugHandle, TapReader, TapWriter)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let byte_counter = Arc::new(AtomicU64::new(0));
    let writer = ProgressWriter {
        inner: writer,
        counter: byte_counter.clone(),
    };
    let writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Unpin + Send>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(writer)));
    let (passthrough_tx, passthrough_rx) = mpsc::channel::<Vec<u8>>(64);
    let (frame_tx, frame_rx) = mpsc::channel::<Frame>(1);
    let (control_tx, control_rx) = mpsc::channel::<TapControl>(4);

    tokio::spawn(read_loop(reader, passthrough_tx, frame_tx, control_rx));

    let (poison_tx, _poison_rx) = watch::channel(0u64);
    let handle = DebugHandle {
        writer: writer.clone(),
        frame_rx: tokio::sync::Mutex::new(frame_rx),
        control_tx,
        byte_counter,
        poison_tx,
    };
    let tap_reader = TapReader {
        rx: passthrough_rx,
        buf: Vec::new(),
        pos: 0,
    };
    let tap_writer = TapWriter { writer };

    (handle, tap_reader, tap_writer)
}

/// Background read loop. Scans incoming bytes and routes them.
async fn read_loop(
    reader: impl AsyncRead + Unpin,
    passthrough_tx: mpsc::Sender<Vec<u8>>,
    frame_tx: mpsc::Sender<Frame>,
    mut control_rx: mpsc::Receiver<TapControl>,
) {
    // eprintln!("[tap] read_loop started");
    let mut reader = BufReader::new(reader);
    let mut noise = Vec::new();
    let mut prev_was_7e = false;

    loop {
        let b = tokio::select! {
            biased;
            ctrl = control_rx.recv() => {
                match ctrl {
                    Some(TapControl::ResyncOnSentinel { sentinel, done }) => {
                        // eprintln!(
                        //     "[tap] resync requested, sentinel={:02x?} ({} bytes), flushing {} noise bytes",
                        //     sentinel, sentinel.len(), noise.len()
                        // );
                        // Flush any accumulated noise before entering resync.
                        if !noise.is_empty() {
                            let _ = passthrough_tx.send(std::mem::take(&mut noise)).await;
                        }
                        prev_was_7e = false;
                        if resync_on_sentinel(&mut reader, &passthrough_tx, &sentinel)
                            .await
                            .is_err()
                        {
                            // eprintln!("[tap] resync_on_sentinel failed (read error), exiting");
                            return;
                        }
                        // eprintln!("[tap] resync complete, resuming normal framing");
                        let _ = done.send(());
                        continue;
                    }
                    None => {
                        eprintln!("[tap] control channel closed, exiting");
                        break;
                    }
                }
            }
            result = reader.read_u8() => {
                match result {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("[tap] read error: {e}, exiting");
                        break;
                    }
                }
            }
        };

        // If the previous byte was 0x7E, check if this byte completes the start
        // marker 0x7E79. If so, route to frame parser
        if prev_was_7e && b == 0x79 {
            // Start marker found. The 0x7E we held back is part of the
            // marker, not noise — don't include it.
            prev_was_7e = false;
            // eprintln!("[tap] frame start marker 0x[7e79] detected");

            // Flush any accumulated noise to passthrough.
            if !noise.is_empty() {
                // if noise.len() <= 16 {
                //     eprintln!(
                //         "[tap] flushing {} noise bytes before frame: {:02x?}",
                //         noise.len(),
                //         noise
                //     );
                // } else {
                //     eprintln!(
                //         "[tap] flushing {} noise bytes before frame: {:02x?}...",
                //         noise.len(),
                //         &noise[..16]
                //     );
                // }
                let _ = passthrough_tx.send(std::mem::take(&mut noise)).await;
            }

            // Read the rest of the frame (length, header, payload).
            match read_frame_body(&mut reader).await {
                Ok(frame) => {
                    // if frame.payload().len() <= 16 {
                    //     eprintln!(
                    //         "[tap] frame complete, {} payload bytes: {:02x?}",
                    //         frame.payload().len(),
                    //         frame.payload()
                    //     );
                    // } else {
                    //     eprintln!(
                    //         "[tap] frame complete, {} payload bytes: {:02x?}...",
                    //         frame.payload().len(),
                    //         &frame.payload()[..16]
                    //     );
                    // }
                    let _ = frame_tx.send(frame).await;
                }
                Err(e) => {
                    eprintln!("[tap] frame body read failed: {e}, exiting");
                }
            }
        } else {
            if prev_was_7e {
                // The previous 0x7E was not a start marker — it's noise.
                // eprintln!("[tap] false 7e (followed by {:02x}), pushing to noise", b);
                noise.push(0x7E);
            }
            if b == 0x7E {
                // eprintln!("[tap] holding 7e (possible start marker)");
                prev_was_7e = true;
            } else {
                prev_was_7e = false;
                noise.push(b);
            }
        }

        // Flush noise when the internal buffer is drained (meaning the
        // next read will actually go to the OS), so passthrough stays
        // responsive without flushing on every single byte.
        if !noise.is_empty() && reader.buffer().is_empty() {
            // if noise.len() <= 16 {
            //     eprintln!(
            //         "[tap] flushing {} noise bytes (buffer drained, prev_7e={}): {:02x?}",
            //         noise.len(),
            //         prev_was_7e,
            //         noise,
            //     );
            // } else {
            //     eprintln!(
            //         "[tap] flushing {} noise bytes (buffer drained, prev_7e={}): {:02x?}...",
            //         noise.len(),
            //         prev_was_7e,
            //         &noise[..16],
            //     );
            // }
            let _ = passthrough_tx.send(std::mem::take(&mut noise)).await;
        }
    }

    // Flush any remaining noise on exit.
    if !noise.is_empty() {
        // eprintln!(
        //     "[tap] flushing {} remaining noise bytes on exit",
        //     noise.len()
        // );
        let _ = passthrough_tx.send(noise).await;
    }
    // eprintln!("[tap] read_loop exited");
}

/// Sentinel-resync sub-loop.
///
/// Forwards every received byte as passthrough noise while matching against
/// `sentinel` with a rolling window. Returns when a contiguous match is
/// found — any unrelated bytes before the match have already been forwarded
/// to `passthrough_tx` so the downstream line reader sees the full sequence.
///
/// Handles self-overlap correctly: on mismatch, the window keeps the longest
/// suffix of the input that is also a prefix of the sentinel.
async fn resync_on_sentinel(
    reader: &mut (impl AsyncRead + Unpin),
    passthrough_tx: &mpsc::Sender<Vec<u8>>,
    sentinel: &[u8],
) -> io::Result<()> {
    if sentinel.is_empty() {
        return Ok(());
    }
    let mut window: Vec<u8> = Vec::with_capacity(sentinel.len());
    loop {
        let mut b = [0u8; 1];
        reader.read_exact(&mut b).await?;
        let byte = b[0];

        // Forward as noise immediately so the downstream line reader sees it.
        let _ = passthrough_tx.send(vec![byte]).await;

        // Extend the window; if it would overflow, drop the front by one and
        // check if the remaining suffix is still a prefix of the sentinel.
        window.push(byte);
        while !window.is_empty() && !sentinel.starts_with(&window) {
            window.remove(0);
        }

        if window.len() == sentinel.len() {
            return Ok(());
        }
    }
}

/// Read the remaining frame fields after the start marker has been consumed.
async fn read_frame_body(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<Frame> {
    let mut len_buf = [0u8; 2];
    reader.read_exact(&mut len_buf).await?;
    let len = u16::from_le_bytes(len_buf) as usize;

    // Channel + CRC (discarded).
    let mut header = [0u8; 2];
    reader.read_exact(&mut header).await?;

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;

    Ok(Frame::new(payload))
}

// ---------------------------------------------------------------------------
// DebugHandle
// ---------------------------------------------------------------------------

/// Send SifliDebug commands and receive responses through the tap.
pub struct DebugHandle {
    writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    frame_rx: tokio::sync::Mutex<mpsc::Receiver<Frame>>,
    control_tx: mpsc::Sender<TapControl>,
    byte_counter: Arc<AtomicU64>,
    poison_tx: watch::Sender<u64>,
}

impl DebugHandle {
    /// Shared atomic counter of bytes written to the underlying writer.
    ///
    /// Monotonically increasing; sample with `load(Ordering::Relaxed)` to
    /// drive a fine-grained progress bar during long writes.
    pub fn byte_counter(&self) -> Arc<AtomicU64> {
        self.byte_counter.clone()
    }

    /// Poison the handle: any in-flight `request()` call (via the watch
    /// channel) and any future request from the current session (via the
    /// generation mismatch) will return `Error::Timeout` immediately.
    ///
    /// Call when the device has rebooted (SFBL seen) and any existing
    /// debug session is known to be dead. The generation bump is sticky
    /// — all requests fail until the next `enter()` snapshots the new
    /// generation.
    pub fn poison(&self) {
        self.poison_tx.send_modify(|g| *g += 1);
    }
}

/// Send a command and wait for the response.
///
/// For fire-and-forget commands (`Exit`), returns `Ok(None)`.
impl DebugHandle {
    pub async fn request(&self, poison_gen: u64, cmd: &Command<'_>) -> Result<Response, Error> {
        if *self.poison_tx.borrow() != poison_gen {
            eprintln!("[dbg] poisoned — rejecting {cmd} without sending");
            return Err(Error::Timeout);
        }

        eprintln!("[dbg] sending command: {}", cmd);

        let frame = cmd.to_frame();
        {
            let mut w = self.writer.lock().await;
            frame.send(&mut **w).await?;
        }

        eprintln!("[dbg] command sent, awaiting response...");

        let mut poison = self.poison_tx.subscribe();
        let frame = {
            let mut rx = self.frame_rx.lock().await;
            tokio::select! {
                biased;
                _ = poison.changed() => {
                    eprintln!("[dbg] poisoned — device rebooted, aborting request");
                    return Err(Error::Timeout);
                }
                frame = rx.recv() => {
                    frame.ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "read task exited"))?
                }
            }
        };

        match Response::parse(frame.payload()) {
            Ok(v) => eprintln!("[dbg] response parsed successfully: {}", v),
            Err(e) => eprintln!(
                "[dbg] response parse error: {e}, raw payload: {:02x?}",
                frame.payload()
            ),
        };

        Ok(Response::parse(frame.payload())?)
    }

    /// Enter debug mode. Returns the poison generation snapshot that
    /// must be passed to subsequent `request()` calls on this session.
    ///
    /// Drains stale response frames, then sends the Enter command.
    pub async fn enter(&self) -> Result<u64, Error> {
        let poison_gen = *self.poison_tx.borrow();
        {
            let mut rx = self.frame_rx.lock().await;
            while rx.try_recv().is_ok() {}
        }
        self.request(poison_gen, &Command::Enter).await?;
        Ok(poison_gen)
    }

    /// Exit debug mode (fire-and-forget).
    pub async fn exit(&self, poison_gen: u64) -> Result<(), Error> {
        self.request(poison_gen, &Command::Exit).await?;
        Ok(())
    }

    /// Read `count` 32-bit words starting at `addr`.
    pub async fn mem_read(
        &self,
        poison_gen: u64,
        addr: u32,
        count: u16,
    ) -> Result<Vec<u32>, Error> {
        match self
            .request(poison_gen, &Command::MemRead { addr, count })
            .await?
        {
            Response::MemRead(words) => Ok(words),
            _ => Err(Error::Protocol(
                super::protocol::ProtocolError::UnexpectedResponse("MemRead"),
            )),
        }
    }

    /// Write 32-bit words to `addr`.
    pub async fn mem_write(&self, poison_gen: u64, addr: u32, data: &[u32]) -> Result<(), Error> {
        match self
            .request(poison_gen, &Command::MemWrite { addr, data })
            .await?
        {
            Response::MemWrite => Ok(()),
            _ => Err(Error::Protocol(
                super::protocol::ProtocolError::UnexpectedResponse("MemWrite"),
            )),
        }
    }

    /// Write 32-bit words to `addr` without waiting for a response.
    /// Use for operations that kill the connection (e.g. soft reset via AIRCR).
    pub async fn mem_write_no_response(&self, addr: u32, data: &[u32]) -> Result<(), Error> {
        let cmd = Command::MemWrite { addr, data };
        let mut w = self.writer.lock().await;
        cmd.to_frame().send(&mut **w).await?;
        Ok(())
    }

    /// Put the tap into sentinel-resync mode with a timeout.
    ///
    /// The tap discards frame parser state and starts forwarding every
    /// received byte as passthrough noise while matching a rolling window
    /// against `sentinel`. Returns Ok once the sentinel is found on the
    /// wire, or `Error::Timeout` if `timeout` elapses first.
    ///
    /// On timeout the tap is **left in resync mode** — when the sentinel
    /// eventually arrives (e.g. user manually resets the device), the
    /// sub-loop completes and the tap returns to normal framing on its
    /// own. The bridge can keep reading bytes via the passthrough path
    /// in the meantime.
    ///
    /// Either way, any stale frames in the receive channel are drained
    /// before returning so the next `request()` call sees a clean slate.
    ///
    /// Use this after issuing a command that disrupts normal framing
    /// (e.g. a soft reset that cuts the device's response mid-frame).
    pub async fn resync_on_sentinel(
        &self,
        sentinel: Vec<u8>,
        timeout: std::time::Duration,
    ) -> Result<(), Error> {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        self.control_tx
            .send(TapControl::ResyncOnSentinel {
                sentinel,
                done: done_tx,
            })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "tap closed"))?;

        let result = tokio::time::timeout(timeout, done_rx).await;

        // Drain stale frames regardless of success/timeout. While the tap
        // is in resync mode no new frames are routed, so this drain is
        // safe even if the sentinel hasn't arrived yet.
        {
            let mut rx = self.frame_rx.lock().await;
            while rx.try_recv().is_ok() {}
        }

        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err(io::Error::new(io::ErrorKind::BrokenPipe, "tap closed").into()),
            Err(_) => Err(Error::Timeout),
        }
    }
}

// ---------------------------------------------------------------------------
// TapReader
// ---------------------------------------------------------------------------

/// Reads non-debug bytes from the wire.
pub struct TapReader {
    rx: mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl AsyncRead for TapReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        // Drain leftover from last chunk.
        if this.pos < this.buf.len() {
            let remaining = &this.buf[this.pos..];
            let n = remaining.len().min(buf.remaining());
            buf.put_slice(&remaining[..n]);
            this.pos += n;
            return Poll::Ready(Ok(()));
        }

        // Poll for next chunk.
        match this.rx.poll_recv(cx) {
            Poll::Ready(Some(chunk)) => {
                let n = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..n]);
                if n < chunk.len() {
                    this.buf = chunk;
                    this.pos = n;
                } else {
                    this.buf.clear();
                    this.pos = 0;
                }
                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "read task exited",
            ))),
            Poll::Pending => Poll::Pending,
        }
    }
}

// ---------------------------------------------------------------------------
// TapWriter
// ---------------------------------------------------------------------------

/// Writes to the wire. Shares the underlying writer with `DebugHandle`.
pub struct TapWriter {
    writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
}

impl TapWriter {
    pub async fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut guard = self.writer.lock().await;
        AsyncWriteExt::write(&mut **guard, buf).await
    }

    pub async fn flush(&mut self) -> io::Result<()> {
        let mut guard = self.writer.lock().await;
        AsyncWriteExt::flush(&mut **guard).await
    }
}
