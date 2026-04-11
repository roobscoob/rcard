use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const START_MARKER: [u8; 2] = [0x7E, 0x79];
const CHANNEL: u8 = 0x10;
const CRC: u8 = 0x00;

/// A raw SifliDebug frame — just a payload with wire format encode/decode.
///
/// Knows nothing about command/response semantics.
#[derive(Debug)]
pub struct Frame {
    payload: Vec<u8>,
}

impl Frame {
    pub fn new(payload: Vec<u8>) -> Self {
        Frame { payload }
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }

    /// Encode and write this frame to `writer`.
    ///
    /// Wire format: `7E 79 | len:u16le | 10 00 | payload`
    pub async fn send(&self, writer: &mut (impl AsyncWrite + Unpin + ?Sized)) -> io::Result<()> {
        let len = self.payload.len() as u16;
        writer.write_all(&START_MARKER).await?;
        writer.write_all(&len.to_le_bytes()).await?;
        writer.write_all(&[CHANNEL, CRC]).await?;
        writer.write_all(&self.payload).await?;
        writer.flush().await
    }

    /// Read a frame from `reader`, scanning for the start marker.
    ///
    /// Returns the frame and any bytes seen before the start marker
    /// (the "noise" — non-protocol traffic on the wire).
    pub async fn recv(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<(Vec<u8>, Frame)> {
        let mut noise = Vec::new();
        let mut prev: Option<u8> = None;

        // Scan for 7E 79.
        loop {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte).await?;
            let b = byte[0];

            if prev == Some(START_MARKER[0]) && b == START_MARKER[1] {
                // Found marker. The 0x7E we buffered into noise was
                // actually part of the marker — remove it.
                if let Some(last) = noise.last() {
                    if *last == START_MARKER[0] {
                        noise.pop();
                    }
                }
                break;
            }

            // Buffer the previous byte as noise (if any).
            if let Some(p) = prev {
                noise.push(p);
            }
            prev = Some(b);
        }

        // Read length (u16 LE).
        let mut len_buf = [0u8; 2];
        reader.read_exact(&mut len_buf).await?;
        let len = u16::from_le_bytes(len_buf) as usize;

        // Read and discard channel + CRC.
        let mut header = [0u8; 2];
        reader.read_exact(&mut header).await?;

        // Read payload.
        let mut payload = vec![0u8; len];
        reader.read_exact(&mut payload).await?;

        Ok((noise, Frame { payload }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    /// Async wrapper around `std::io::Cursor` for testing.
    struct Cursor(std::io::Cursor<Vec<u8>>);

    impl Cursor {
        fn new(data: Vec<u8>) -> Self {
            Self(std::io::Cursor::new(data))
        }

        fn into_inner(self) -> Vec<u8> {
            self.0.into_inner()
        }
    }

    impl AsyncRead for Cursor {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            use std::io::Read;
            let n = Read::read(&mut self.0, buf.initialize_unfilled())?;
            buf.advance(n);
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for Cursor {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            use std::io::Write;
            Poll::Ready(Write::write(&mut self.0, buf))
        }

        fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            use std::io::Write;
            Poll::Ready(Write::flush(&mut self.0))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn round_trip() {
        let frame = Frame::new(vec![0xAA, 0xBB, 0xCC]);
        let mut wire = Cursor::new(Vec::new());
        frame.send(&mut wire).await.unwrap();

        let mut cursor = Cursor::new(wire.into_inner());
        let (noise, decoded) = Frame::recv(&mut cursor).await.unwrap();
        assert!(noise.is_empty());
        assert_eq!(decoded.payload(), &[0xAA, 0xBB, 0xCC]);
    }

    #[tokio::test]
    async fn noise_before_frame() {
        let frame = Frame::new(vec![0xDD]);
        let mut wire_cursor = Cursor::new(Vec::new());
        frame.send(&mut wire_cursor).await.unwrap();

        let mut wire = b"SFBL\r\n".to_vec();
        wire.extend_from_slice(&wire_cursor.into_inner());

        let mut cursor = Cursor::new(wire);
        let (noise, decoded) = Frame::recv(&mut cursor).await.unwrap();
        assert_eq!(noise, b"SFBL\r\n");
        assert_eq!(decoded.payload(), &[0xDD]);
    }

    #[tokio::test]
    async fn enter_frame_encoding() {
        let payload = vec![0x41, 0x54, 0x53, 0x46, 0x33, 0x32, 0x05, 0x21];
        let frame = Frame::new(payload);
        let mut wire = Cursor::new(Vec::new());
        frame.send(&mut wire).await.unwrap();
        assert_eq!(
            wire.into_inner(),
            [
                0x7E, 0x79, 0x08, 0x00, 0x10, 0x00, 0x41, 0x54, 0x53, 0x46, 0x33, 0x32, 0x05, 0x21
            ]
        );
    }

    #[tokio::test]
    async fn false_start_marker() {
        // A 0x7E in the noise that isn't followed by 0x79.
        let frame = Frame::new(vec![0x01]);
        let mut wire_cursor = Cursor::new(Vec::new());
        frame.send(&mut wire_cursor).await.unwrap();

        let mut wire = vec![0x7E, 0x00]; // false start
        wire.extend_from_slice(&wire_cursor.into_inner());

        let mut cursor = Cursor::new(wire);
        let (noise, decoded) = Frame::recv(&mut cursor).await.unwrap();
        assert_eq!(noise, &[0x7E, 0x00]);
        assert_eq!(decoded.payload(), &[0x01]);
    }
}
