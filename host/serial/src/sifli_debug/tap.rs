use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::sync::mpsc;

use super::frame::Frame;
use super::protocol::{Command, Error, Response};

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
    let writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Unpin + Send>>> =
        Arc::new(tokio::sync::Mutex::new(Box::new(writer)));
    let (passthrough_tx, passthrough_rx) = mpsc::channel::<Vec<u8>>(64);
    let (frame_tx, frame_rx) = mpsc::channel::<Frame>(1);

    tokio::spawn(read_loop(reader, passthrough_tx, frame_tx));

    let handle = DebugHandle {
        writer: writer.clone(),
        frame_rx: tokio::sync::Mutex::new(frame_rx),
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
) {
    let mut reader = BufReader::new(reader);
    let mut noise = Vec::new();
    let mut prev_was_7e = false;

    loop {
        let mut byte_buf = [0u8; 1];
        let b = match reader.read_exact(&mut byte_buf).await {
            Ok(_) => byte_buf[0],
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                eprintln!("[tap] read task: got EOF");
                break;
            }
            Err(e) => {
                eprintln!("[tap] read task: read error: {e}");
                break;
            }
        };

        if prev_was_7e && b == 0x79 {
            // Start marker found. The 0x7E we held back is part of the
            // marker, not noise — don't include it.
            prev_was_7e = false;

            // Flush any accumulated noise to passthrough.
            if !noise.is_empty() {
                let _ = passthrough_tx.send(std::mem::take(&mut noise)).await;
            }

            // Read the rest of the frame (length, header, payload).
            match read_frame_body(&mut reader).await {
                Ok(frame) => {
                    let _ = frame_tx.send(frame).await;
                }
                Err(e) => {
                    eprintln!("[tap] read task: frame body error: {e}");
                    return;
                }
            }
        } else {
            if prev_was_7e {
                // The previous 0x7E was not a start marker — it's noise.
                noise.push(0x7E);
            }
            if b == 0x7E {
                prev_was_7e = true;
            } else {
                prev_was_7e = false;
                noise.push(b);
            }
        }

        // Flush noise when the internal buffer is drained (meaning the
        // next read will actually go to the OS), so passthrough stays
        // responsive without flushing on every single byte.
        if !noise.is_empty() && !prev_was_7e && reader.buffer().is_empty() {
            let _ = passthrough_tx.send(std::mem::take(&mut noise)).await;
        }
    }

    // Flush any remaining noise on exit.
    if !noise.is_empty() {
        let _ = passthrough_tx.send(noise).await;
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
}

/// Send a command and wait for the response.
///
/// For fire-and-forget commands (`Exit`), returns `Ok(None)`.
impl DebugHandle {
    pub async fn request(&self, cmd: &Command<'_>) -> Result<Option<Response>, Error> {
        {
            let mut w = self.writer.lock().await;
            cmd.to_frame().send(&mut **w).await?;
        }

        if !cmd.expects_response() {
            return Ok(None);
        }

        let frame = self
            .frame_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "read task exited"))?;

        Ok(Some(Response::parse(frame.payload())?))
    }

    /// Enter debug mode.
    pub async fn enter(&self) -> Result<(), Error> {
        self.request(&Command::Enter).await?;
        Ok(())
    }

    /// Exit debug mode (fire-and-forget).
    pub async fn exit(&self) -> Result<(), Error> {
        self.request(&Command::Exit).await?;
        Ok(())
    }

    /// Read `count` 32-bit words starting at `addr`.
    pub async fn mem_read(&self, addr: u32, count: u16) -> Result<Vec<u32>, Error> {
        let resp = self.request(&Command::MemRead { addr, count }).await?;
        match resp {
            Some(Response::MemRead(words)) => Ok(words),
            _ => unreachable!("MemRead always expects a response"),
        }
    }

    /// Write 32-bit words to `addr`.
    pub async fn mem_write(&self, addr: u32, data: &[u32]) -> Result<(), Error> {
        self.request(&Command::MemWrite { addr, data }).await?;
        Ok(())
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
