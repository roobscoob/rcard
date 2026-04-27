use std::sync::Arc;
use std::time::Instant;

use tokio::io::AsyncReadExt;

use crate::capability::SifliDebug;
use crate::sifli_debug::{self, TapReader};

/// Raw USART1 connection — text reader + SifliDebug handle.
/// The caller drives the read loop, which allows intercepting
/// the text stream (e.g. for sentinel detection).
pub struct Usart1Connection {
    pub reader: TapReader,
    pub sifli_debug: Arc<SifliDebug>,
}

impl Usart1Connection {
    /// Open a USART1 serial port and split into text reader + debug handle.
    pub fn open(port: &str) -> Result<Self, serialport::Error> {
        let stream = tokio_serial::SerialStream::open(&tokio_serial::new(port, 1_000_000))?;
        let (reader, writer) = tokio::io::split(stream);
        let (handle, tap_reader, _tap_writer) = sifli_debug::tap(reader, writer);
        Ok(Usart1Connection {
            reader: tap_reader,
            sifli_debug: Arc::new(SifliDebug::new(Arc::new(handle))),
        })
    }

    /// Read the next line from the text stream.
    ///
    /// Returns the host `Instant` at which the *first byte* of the line
    /// was observed, or `None` on EOF or error. The caller uses this
    /// timestamp as the log's `received_at` so multi-adapter ordering
    /// reflects real byte-arrival times, not event-dispatch times.
    pub async fn read_line(&mut self, line_buf: &mut String) -> Option<Instant> {
        let mut buf = [0u8; 1];
        let mut line_start: Option<Instant> = None;
        loop {
            match self.reader.read(&mut buf).await {
                Ok(0) => return None,
                Ok(_) => {
                    if buf[0] == b'\n' {
                        return Some(line_start.unwrap_or_else(Instant::now));
                    } else {
                        if line_start.is_none() {
                            line_start = Some(Instant::now());
                        }
                        line_buf.push(buf[0] as char);
                    }
                }
                Err(e) => {
                    eprintln!("Error reading from USART1: {e}");
                    return None;
                }
            }
        }
    }
}
