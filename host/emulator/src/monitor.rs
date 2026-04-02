use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::EmulatorError;

// ── Protocol phases ─────────────────────────────────────────────────

enum Phase {
    /// Consuming the initial banner until the first prompt.
    Banner,
    /// Prompt received; ready for the next command.
    Idle,
    /// Command sent; waiting for Renode to echo it back.
    AwaitEcho(String),
    /// Echo matched; accumulating response lines until the next prompt.
    AwaitPrompt,
    /// TCP stream closed or unrecoverable timeout.
    Closed,
}

// ── Shared state (Mutex + Condvar) ──────────────────────────────────

struct Inner {
    phase: Phase,
    /// Response lines collected during `AwaitPrompt`.
    response: Vec<String>,
    /// Byte accumulator for the current (unterminated) line.
    partial: Vec<u8>,
    /// Remaining bytes to skip for an in-progress telnet IAC sequence.
    iac_skip: u8,
    /// True while inside an ANSI escape sequence (ESC … letter).
    in_ansi: bool,
    /// The prompt bytes we expect, e.g. `b"(monitor) "`.
    prompt: Vec<u8>,
}

impl Inner {
    fn new() -> Self {
        Self {
            phase: Phase::Banner,
            response: Vec::new(),
            partial: Vec::new(),
            iac_skip: 0,
            in_ansi: false,
            prompt: b"(monitor) ".to_vec(),
        }
    }

    /// Feed one byte from the TCP stream into the state machine.
    fn feed(&mut self, b: u8) {
        // Telnet IAC: skip the 2 bytes following 0xFF.
        if self.iac_skip > 0 {
            self.iac_skip -= 1;
            return;
        }
        if b == 0xFF {
            self.iac_skip = 2;
            return;
        }

        // ANSI escape: ESC … <letter>.
        if b == 0x1B {
            self.in_ansi = true;
            return;
        }
        if self.in_ansi {
            if b.is_ascii_alphabetic() {
                self.in_ansi = false;
            }
            return;
        }

        match self.phase {
            Phase::Banner => {
                if b == b'\n' {
                    self.partial.clear();
                } else {
                    self.partial.push(b);
                    if self.at_prompt() {
                        self.partial.clear();
                        self.phase = Phase::Idle;
                    }
                }
            }

            Phase::AwaitEcho(_) => {
                if b == b'\n' {
                    let line =
                        strip_control_str(&String::from_utf8_lossy(&self.partial));
                    self.partial.clear();
                    let matches = match self.phase {
                        Phase::AwaitEcho(ref cmd) => line.trim() == cmd.as_str(),
                        _ => false,
                    };
                    if matches {
                        self.phase = Phase::AwaitPrompt;
                    }
                } else {
                    self.partial.push(b);
                }
            }

            Phase::AwaitPrompt => {
                if b == b'\n' {
                    let line =
                        String::from_utf8_lossy(&self.partial).into_owned();
                    self.response.push(line);
                    self.partial.clear();
                } else {
                    self.partial.push(b);
                    if self.at_prompt() {
                        self.partial.clear();
                        self.phase = Phase::Idle;
                    }
                }
            }

            Phase::Idle => {
                // Discard unexpected data while idle.
                if b == b'\n' {
                    self.partial.clear();
                }
            }

            Phase::Closed => {}
        }
    }

    /// True when `partial` exactly matches the expected prompt.
    fn at_prompt(&self) -> bool {
        let p = &self.partial;
        let start = if p.first() == Some(&b'\r') { 1 } else { 0 };
        p.get(start..) == Some(self.prompt.as_slice())
    }
}

// ── Public API ──────────────────────────────────────────────────────

pub struct Monitor {
    shared: Arc<(Mutex<Inner>, Condvar)>,
    writer: Mutex<TcpStream>,
    _reader: JoinHandle<()>,
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

        let writer = stream.try_clone().map_err(EmulatorError::MonitorConnect)?;
        let shared = Arc::new((Mutex::new(Inner::new()), Condvar::new()));

        let r_shared = Arc::clone(&shared);
        let reader =
            std::thread::spawn(move || reader_loop(stream, r_shared));

        let mon = Monitor {
            shared,
            writer: Mutex::new(writer),
            _reader: reader,
        };

        // Wait for the banner to be consumed (transitions to Idle).
        mon.wait_idle(timeout.saturating_sub(start.elapsed()))?;
        Ok(mon)
    }

    /// Send a command; block until Renode has finished processing it.
    pub fn send(&self, cmd: &str) -> Result<(), EmulatorError> {
        self.execute(cmd, Duration::from_secs(30))?;
        Ok(())
    }

    /// Fire a command without waiting for a response (used in Drop paths).
    pub fn send_nowait(&self, cmd: &str) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(cmd.as_bytes());
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
    }

    /// Send a command and return the stripped response.
    pub fn query(&self, cmd: &str) -> Result<String, EmulatorError> {
        self.query_with_timeout(cmd, Duration::from_secs(30))
    }

    /// Like `query`, but with a custom timeout for slow operations.
    pub fn query_with_timeout(
        &self,
        cmd: &str,
        timeout: Duration,
    ) -> Result<String, EmulatorError> {
        let raw = self.execute(cmd, timeout)?;
        let mut out = Vec::new();
        for line in &raw {
            let clean = strip_control_str(line);
            let trimmed = clean.trim().to_string();
            if !trimmed.is_empty() {
                out.push(trimmed);
            }
        }
        Ok(out.join("\n"))
    }

    // ── Internals ───────────────────────────────────────────────────

    /// Core primitive: send `cmd`, wait for the next prompt, return the
    /// raw response lines collected between the echo and the prompt.
    fn execute(
        &self,
        cmd: &str,
        timeout: Duration,
    ) -> Result<Vec<String>, EmulatorError> {
        let (lock, cvar) = &*self.shared;
        let deadline = Instant::now() + timeout;

        // 1. Wait for Idle.
        {
            let mut inner = lock.lock().unwrap();
            loop {
                match inner.phase {
                    Phase::Idle => break,
                    Phase::Closed => return Err(EmulatorError::MonitorDisconnected),
                    _ => {}
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(EmulatorError::MonitorCommand(format!(
                        "timed out waiting to send '{cmd}'"
                    )));
                }
                let (g, _) = cvar.wait_timeout(inner, remaining).unwrap();
                inner = g;
            }

            // 2. Prepare and transition to AwaitEcho.
            inner.response.clear();
            if let Some(name) = parse_mach_create(cmd) {
                inner.prompt = format!("({name}) ").into_bytes();
            }
            inner.phase = Phase::AwaitEcho(cmd.to_string());
        }

        // 3. Write the command (outside the inner lock so the reader
        //    thread can process bytes while we write).
        {
            let mut w = self.writer.lock().unwrap();
            w.write_all(cmd.as_bytes())
                .map_err(EmulatorError::MonitorSend)?;
            w.write_all(b"\n")
                .map_err(EmulatorError::MonitorSend)?;
            w.flush().map_err(EmulatorError::MonitorSend)?;
        }

        // 4. Wait for the state machine to reach Idle again.
        {
            let mut inner = lock.lock().unwrap();
            loop {
                match inner.phase {
                    Phase::Idle => {
                        return Ok(std::mem::take(&mut inner.response))
                    }
                    Phase::Closed => {
                        return Err(EmulatorError::MonitorDisconnected)
                    }
                    _ => {}
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    inner.phase = Phase::Closed;
                    return Err(EmulatorError::MonitorCommand(format!(
                        "timed out waiting for response to '{cmd}'"
                    )));
                }
                let (g, _) = cvar.wait_timeout(inner, remaining).unwrap();
                inner = g;
            }
        }
    }

    /// Block until phase is `Idle` (used during connect for the banner).
    fn wait_idle(&self, timeout: Duration) -> Result<(), EmulatorError> {
        let (lock, cvar) = &*self.shared;
        let mut inner = lock.lock().unwrap();
        let deadline = Instant::now() + timeout;
        loop {
            match inner.phase {
                Phase::Idle => return Ok(()),
                Phase::Closed => return Err(EmulatorError::MonitorDisconnected),
                _ => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(EmulatorError::MonitorCommand(
                    "timed out waiting for Renode banner".into(),
                ));
            }
            let (g, _) = cvar.wait_timeout(inner, remaining).unwrap();
            inner = g;
        }
    }
}

// ── Reader thread ───────────────────────────────────────────────────

fn reader_loop(
    stream: TcpStream,
    shared: Arc<(Mutex<Inner>, Condvar)>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .ok();

    let (lock, cvar) = &*shared;
    let mut buf = [0u8; 1024];

    loop {
        match (&stream).read(&mut buf) {
            Ok(0) => {
                lock.lock().unwrap().phase = Phase::Closed;
                cvar.notify_all();
                break;
            }
            Ok(n) => {
                let mut inner = lock.lock().unwrap();
                let was_idle = matches!(inner.phase, Phase::Idle);
                for &b in &buf[..n] {
                    inner.feed(b);
                }
                if !was_idle && matches!(inner.phase, Phase::Idle) {
                    drop(inner);
                    cvar.notify_all();
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                if matches!(lock.lock().unwrap().phase, Phase::Closed) {
                    break;
                }
            }
            Err(_) => {
                lock.lock().unwrap().phase = Phase::Closed;
                cvar.notify_all();
                break;
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Extract the machine name from `mach create "name"`.
fn parse_mach_create(cmd: &str) -> Option<&str> {
    let rest = cmd.trim().strip_prefix("mach create")?.trim();
    rest.strip_prefix('"')?.strip_suffix('"')
}

/// Strip ANSI escapes, telnet IAC sequences, and control characters.
fn strip_control_str(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0xFF {
            i += 3;
            continue;
        }
        if b == 0x1B {
            i += 1;
            while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                i += 1;
            }
            i += 1;
            continue;
        }
        if b >= 0x20 {
            out.push(b as char);
        }
        i += 1;
    }
    out
}
