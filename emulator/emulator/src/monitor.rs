use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::EmulatorError;

pub struct Monitor {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Monitor {
    /// Connect to Renode's telnet monitor, retrying until `timeout`.
    pub fn connect(port: u16, timeout: Duration) -> Result<Self, EmulatorError> {
        let start = Instant::now();
        let addr = format!("127.0.0.1:{port}");
        let stream = loop {
            match TcpStream::connect(&addr) {
                Ok(s) => break s,
                Err(e) => {
                    if start.elapsed() > timeout {
                        return Err(EmulatorError::MonitorConnect(e));
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        };

        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .ok();

        let writer = stream.try_clone().map_err(EmulatorError::MonitorConnect)?;
        let mut mon = Monitor {
            reader: BufReader::new(stream),
            writer,
        };

        // Consume the initial banner/prompt
        mon.read_until_prompt()?;
        Ok(mon)
    }

    /// Send a command and wait for the prompt to return.
    pub fn send(&mut self, cmd: &str) -> Result<String, EmulatorError> {
        self.writer
            .write_all(cmd.as_bytes())
            .map_err(EmulatorError::MonitorSend)?;
        self.writer
            .write_all(b"\n")
            .map_err(EmulatorError::MonitorSend)?;
        self.writer.flush().map_err(EmulatorError::MonitorSend)?;
        self.read_until_prompt()
    }

    /// Read bytes until we see a Renode prompt line (ending with `> `).
    fn read_until_prompt(&mut self) -> Result<String, EmulatorError> {
        let mut output = String::new();
        loop {
            let mut raw = Vec::new();
            match self.reader.read_until(b'\n', &mut raw) {
                Ok(0) => {
                    return Err(EmulatorError::MonitorDisconnected);
                }
                Ok(_) => {
                    // Strip telnet IAC sequences and ANSI escapes from raw bytes
                    let clean = strip_control(&raw);
                    if clean.trim().ends_with('>') || clean.contains("(monitor)") {
                        break;
                    }
                    output.push_str(&clean);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    // Prompt might already be on a partial line; check what we have
                    break;
                }
                Err(e) => {
                    return Err(EmulatorError::MonitorSend(e));
                }
            }
        }
        Ok(output)
    }
}

/// Strip ANSI escape sequences and telnet IAC bytes from raw bytes.
fn strip_control(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0xFF {
            // Telnet IAC: skip 3 bytes
            i += 3;
            continue;
        }
        if b == 0x1B {
            // ANSI escape: skip until letter
            i += 1;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            i += 1; // skip the final letter
            continue;
        }
        if b >= 0x20 || b == b'\n' || b == b'\r' || b == b'\t' {
            out.push(b as char);
        }
        i += 1;
    }
    out
}
