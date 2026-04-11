use std::any::{Any, TypeId};
use std::sync::Arc;

use device::adapter::{Adapter, AdapterId};
use device::device::LogSink;
use tokio::io::AsyncReadExt;

use crate::capability::SifliDebug;
use crate::sifli_debug::{self, TapReader};

/// A raw USART1 connection — text reader + SifliDebug handle, before
/// being wrapped as an Adapter. Use this when you need to intercept
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
    /// Returns None on EOF or error.
    pub async fn read_line(&mut self, line_buf: &mut String) -> Option<()> {
        let mut buf = [0u8; 1];
        loop {
            match self.reader.read(&mut buf).await {
                Ok(0) => return None,
                Ok(_) => {
                    if buf[0] == b'\n' {
                        return Some(());
                    } else {
                        line_buf.push(buf[0] as char);
                    }
                }
                Err(_) => return None,
            }
        }
    }
}

/// USART1 serial adapter — hypervisor text stream + SifliDebug interface.
///
/// Opens a serial port at 1M baud. The SifliDebug protocol is multiplexed
/// on the same wire; a tap layer separates debug frames from text data.
pub struct Usart1 {
    id: AdapterId,
    sifli_debug: Arc<SifliDebug>,
    _reader_task: tokio::task::JoinHandle<()>,
}

impl Usart1 {
    /// Connect to a USART1 serial port.
    ///
    /// Log events are pushed into the provided `sink`.
    pub fn connect(port: &str, id: AdapterId, sink: LogSink) -> Result<Self, serialport::Error> {
        let stream = tokio_serial::SerialStream::open(&tokio_serial::new(port, 1_000_000))?;
        let (reader, writer) = tokio::io::split(stream);
        let (handle, tap_reader, _tap_writer) = sifli_debug::tap(reader, writer);
        let task = tokio::spawn(read_hypervisor(tap_reader, sink));
        Ok(Usart1 {
            id,
            sifli_debug: Arc::new(SifliDebug::new(Arc::new(handle))),
            _reader_task: task,
        })
    }
}

impl Adapter for Usart1 {
    fn id(&self) -> AdapterId {
        self.id
    }

    fn display_name(&self) -> &str {
        "USART1"
    }

    fn capabilities(&self) -> Vec<(TypeId, Arc<dyn Any + Send + Sync>)> {
        vec![(TypeId::of::<SifliDebug>(), self.sifli_debug.clone())]
    }
}

impl Drop for Usart1 {
    fn drop(&mut self) {
        self._reader_task.abort();
    }
}

/// Read UTF-8 lines from USART1 and push them into the device via LogSink.
async fn read_hypervisor(mut reader: TapReader, sink: LogSink) {
    use crate::error::SerialError;

    let mut buf = [0u8; 1024];
    let mut line = String::new();

    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) => {
                sink.error(SerialError::PortClosed);
                return;
            }
            Ok(n) => n,
            Err(e) => {
                sink.error(SerialError::Io(e));
                return;
            }
        };

        for &byte in &buf[..n] {
            if byte == b'\n' {
                let text = std::mem::take(&mut line);
                sink.text(text);
            } else {
                line.push(byte as char);
            }
        }
    }
}
