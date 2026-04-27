use std::fmt::{self, Display};

use super::frame::Frame;

// Response codes from the chip.
const RESP_EXIT: u8 = 0xD0;
const RESP_ENTER: u8 = 0xD1;
const RESP_MEM_READ: u8 = 0xD2;
const RESP_MEM_WRITE: u8 = 0xD3;

// Enter/Exit payload constants.
const ATSF32_PREFIX: &[u8] = b"ATSF32";
const ENTER_SUFFIX: &[u8] = &[0x05, 0x21]; // ENQ + '!'
const EXIT_SUFFIX: &[u8] = &[0x18, 0x21]; // CAN + '!'

// MEMRead/MEMWrite opcode bytes.
const OP_MEM_READ: &[u8] = b"@r";
const OP_MEM_WRITE: &[u8] = b"@w";

/// A SifliDebug command to send to the chip.
#[derive(Debug)]
pub enum Command<'a> {
    Enter,
    Exit,
    MemRead { addr: u32, count: u16 },
    MemWrite { addr: u32, data: &'a [u32] },
}

impl Display for Command<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Enter => write!(f, "Enter"),
            Command::Exit => write!(f, "Exit"),
            Command::MemRead { addr, count } => {
                write!(f, "MemRead(addr: 0x{addr:08X}, count: {count})")
            }
            Command::MemWrite { addr, data } => {
                if data.len() <= 4 {
                    write!(
                        f,
                        "MemWrite(addr: 0x{addr:08X}, data: {:08X?})",
                        data
                    )
                } else {
                    write!(
                        f,
                        "MemWrite(addr: 0x{addr:08X}, data: {:08X?}... [{} words])",
                        &data[..4],
                        data.len()
                    )
                }
            }
        }
    }
}

impl Command<'_> {
    /// Build a `Frame` from this command.
    pub fn to_frame(&self) -> Frame {
        Frame::new(self.to_payload())
    }

    fn to_payload(&self) -> Vec<u8> {
        match self {
            Command::Enter => {
                let mut p = Vec::with_capacity(8);
                p.extend_from_slice(ATSF32_PREFIX);
                p.extend_from_slice(ENTER_SUFFIX);
                p
            }
            Command::Exit => {
                let mut p = Vec::with_capacity(8);
                p.extend_from_slice(ATSF32_PREFIX);
                p.extend_from_slice(EXIT_SUFFIX);
                p
            }
            Command::MemRead { addr, count } => {
                let mut p = Vec::with_capacity(8);
                p.extend_from_slice(OP_MEM_READ);
                p.extend_from_slice(&addr.to_le_bytes());
                p.extend_from_slice(&count.to_le_bytes());
                p
            }
            Command::MemWrite { addr, data } => {
                let mut p = Vec::with_capacity(8 + data.len() * 4);
                p.extend_from_slice(OP_MEM_WRITE);
                p.extend_from_slice(&addr.to_le_bytes());
                p.extend_from_slice(&(data.len() as u16).to_le_bytes());
                for word in *data {
                    p.extend_from_slice(&word.to_le_bytes());
                }
                p
            }
        }
    }
}

/// A parsed response from the chip.
#[derive(Debug)]
pub enum Response {
    /// Exit acknowledged (0xD0).
    Exit,
    /// Enter acknowledged (0xD1).
    Enter,
    /// Memory read result (0xD2).
    MemRead(Vec<u32>),
    /// Memory write acknowledged (0xD3).
    MemWrite,
}

impl Display for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Response::Exit => write!(f, "Exit"),
            Response::Enter => write!(f, "Enter"),
            Response::MemRead(words) => write!(f, "MemRead(count: {})", words.len()),
            Response::MemWrite => write!(f, "MemWrite"),
        }
    }
}

impl Response {
    /// Parse a response from a frame's payload bytes.
    pub fn parse(payload: &[u8]) -> Result<Self, ProtocolError> {
        let &code = payload.first().ok_or(ProtocolError::EmptyPayload)?;
        let body = &payload[1..];

        match code {
            RESP_EXIT => Ok(Response::Exit),
            RESP_ENTER => Ok(Response::Enter),
            RESP_MEM_WRITE => Ok(Response::MemWrite),
            RESP_MEM_READ => {
                // Body is N×4 data bytes + 1 checksum byte (ignored).
                if body.is_empty() {
                    return Err(ProtocolError::PayloadTooShort);
                }
                // Strip the trailing checksum byte.
                let data_bytes = &body[..body.len() - 1];
                if data_bytes.len() % 4 != 0 {
                    return Err(ProtocolError::UnalignedReadData);
                }
                let words: Vec<u32> = data_bytes
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                    .collect();
                Ok(Response::MemRead(words))
            }
            _ => Err(ProtocolError::UnknownResponseCode(code)),
        }
    }
}

/// Errors from protocol-level parsing.
#[derive(Debug)]
pub enum ProtocolError {
    EmptyPayload,
    PayloadTooShort,
    UnalignedReadData,
    UnknownResponseCode(u8),
    UnexpectedResponse(&'static str),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::EmptyPayload => write!(f, "empty response payload"),
            ProtocolError::PayloadTooShort => write!(f, "response payload too short"),
            ProtocolError::UnalignedReadData => {
                write!(f, "MemRead data not aligned to 4 bytes")
            }
            ProtocolError::UnknownResponseCode(c) => {
                write!(f, "unknown response code: 0x{c:02X}")
            }
            ProtocolError::UnexpectedResponse(expected) => {
                write!(f, "unexpected response (expected {expected})")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Top-level error for SifliDebug operations.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Protocol(ProtocolError),
    Timeout,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Protocol(e) => write!(f, "protocol: {e}"),
            Error::Timeout => write!(f, "timeout"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Protocol(e) => Some(e),
            Error::Timeout => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<ProtocolError> for Error {
    fn from(e: ProtocolError) -> Self {
        Error::Protocol(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_payload() {
        let frame = Command::Enter.to_frame();
        assert_eq!(
            frame.payload(),
            &[0x41, 0x54, 0x53, 0x46, 0x33, 0x32, 0x05, 0x21]
        );
    }

    #[test]
    fn exit_payload() {
        let frame = Command::Exit.to_frame();
        assert_eq!(
            frame.payload(),
            &[0x41, 0x54, 0x53, 0x46, 0x33, 0x32, 0x18, 0x21]
        );
    }

    #[test]
    fn mem_read_payload() {
        let frame = Command::MemRead {
            addr: 0xE000EDF0,
            count: 1,
        }
        .to_frame();
        assert_eq!(
            frame.payload(),
            &[0x40, 0x72, 0xF0, 0xED, 0x00, 0xE0, 0x01, 0x00]
        );
    }

    #[test]
    fn mem_write_payload() {
        let frame = Command::MemWrite {
            addr: 0xE000EDF0,
            data: &[0xA05F0003],
        }
        .to_frame();
        assert_eq!(
            frame.payload(),
            &[
                0x40, 0x77, 0xF0, 0xED, 0x00, 0xE0, 0x01, 0x00, 0x03, 0x00, 0x5F, 0xA0
            ]
        );
    }

    #[test]
    fn parse_enter_response() {
        let resp = Response::parse(&[0xD1]).unwrap();
        assert!(matches!(resp, Response::Enter));
    }

    #[test]
    fn parse_mem_write_response() {
        let resp = Response::parse(&[0xD3]).unwrap();
        assert!(matches!(resp, Response::MemWrite));
    }

    #[test]
    fn parse_mem_read_response() {
        // 1 word (0x12345678) + 1 checksum byte.
        let resp = Response::parse(&[0xD2, 0x78, 0x56, 0x34, 0x12, 0x00]).unwrap();
        match resp {
            Response::MemRead(words) => assert_eq!(words, vec![0x12345678]),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_empty_payload() {
        let err = Response::parse(&[]).unwrap_err();
        assert!(matches!(err, ProtocolError::EmptyPayload));
    }

    #[test]
    fn parse_unknown_code() {
        let err = Response::parse(&[0xFF]).unwrap_err();
        assert!(matches!(err, ProtocolError::UnknownResponseCode(0xFF)));
    }
}
