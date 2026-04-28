//! Type-safe client-side IPC call builder.
//!
//! The codegen constructs an `IpcCall`, adds arguments and leases via
//! typed methods, and calls `.send()` to execute the IPC round-trip.
//! The builder handles serialization, the kernel send, and response
//! deserialization. Codegen never touches `sys_send` directly.

use core::cell::UnsafeCell;

use crate::kern;

// ---------------------------------------------------------------------------
// Static IPC buffer — shared across all IPC calls within a single task.
//
// SAFETY: Hubris tasks are single-threaded with no preemption in userspace.
// The panic handler may overwrite this during a panic, but the original
// call's result is abandoned at that point (the task is dying).
// ---------------------------------------------------------------------------

struct IpcBuf(UnsafeCell<[u8; crate::HUBRIS_MESSAGE_SIZE_LIMIT]>);
unsafe impl Sync for IpcBuf {}

static IPC_BUF: IpcBuf = IpcBuf(UnsafeCell::new([0u8; crate::HUBRIS_MESSAGE_SIZE_LIMIT]));

#[inline(always)]
pub fn ipc_buf() -> *mut [u8; crate::HUBRIS_MESSAGE_SIZE_LIMIT] {
    IPC_BUF.0.get()
}

// ---------------------------------------------------------------------------
// call_send — takes a caller-provided buffer (unified arg+reply)
// ---------------------------------------------------------------------------

/// Consolidates `sys_send` + rc-checks + dead-gen update.
///
/// `buf` is used for both outgoing args (`buf[..arg_len]`) and incoming
/// reply (kernel overwrites `buf`). The kernel copies outgoing bytes to
/// the server before writing the reply back, so reuse is safe.
///
/// Returns the number of reply bytes in `buf` on success.
pub fn call_send(
    server_id: &'static crate::StaticTaskId,
    target: kern::TaskId,
    opcode: u16,
    buf: &mut [u8],
    arg_len: usize,
    leases: &mut [kern::Lease<'_>],
) -> Result<usize, crate::Error> {
    let (rc, len) = kern::sys_send(target, opcode, buf, arg_len, leases).map_err(|dead| {
        server_id.set(target.with_generation(dead.new_generation()));
        crate::Error::ServerDied
    })?;
    if rc == crate::ACCESS_VIOLATION {
        crate::__ipc_panic!(
            "ipc: server {} rejected our message: access violation \
             (this task is not authorized to use this server)",
            target,
        );
    }
    if rc != kern::ResponseCode::SUCCESS {
        crate::__ipc_panic!(
            "ipc: server {} sent unexpected non-SUCCESS response code: {}",
            target,
            rc.0
        );
    }
    Ok(len)
}

// ---------------------------------------------------------------------------
// call_send_unified — uses the static IPC_BUF (args already serialized there)
// ---------------------------------------------------------------------------

/// Like `call_send`, but operates on the static `IPC_BUF`.
///
/// The caller must have already serialized args into `ipc_buf()` before
/// calling this. Reply data remains in `IPC_BUF` after return — the caller
/// reads it via `ipc_buf()` before the next IPC call.
pub fn call_send_unified(
    server_id: &'static crate::StaticTaskId,
    target: kern::TaskId,
    opcode: u16,
    arg_len: usize,
    leases: &mut [kern::Lease<'_>],
) -> Result<usize, crate::Error> {
    let buf = unsafe { &mut *ipc_buf() };
    call_send(server_id, target, opcode, buf, arg_len, leases)
}

// ---------------------------------------------------------------------------
// Reply envelope parsing — shared across all generated client methods
// ---------------------------------------------------------------------------

/// Parse the standard reply envelope: tag(0=Ok, 1=Err) + payload.
///
/// Returns `Ok(offset)` where `offset` is the start of the Ok payload
/// (i.e. 1, past the tag byte). Returns `Err(Error)` if the server sent
/// an error reply. Panics on protocol violations (empty reply, malformed
/// error, invalid tag).
pub fn parse_reply_envelope(
    retbuffer: &[u8],
    len: usize,
    server: kern::TaskId,
) -> Result<usize, crate::Error> {
    if len == 0 || len > retbuffer.len() {
        crate::__ipc_panic!("ipc: server {} sent bad reply len {}", server, len);
    }
    // SAFETY: 0 < len <= retbuffer.len() proven above.
    let tag = unsafe { *retbuffer.get_unchecked(0) };
    match tag {
        0u8 => Ok(1),
        1u8 => {
            let err_slice = unsafe { retbuffer.get_unchecked(1..len) };
            let Ok((err, _)) = crate::__postcard::take_from_bytes::<crate::Error>(err_slice)
            else {
                crate::__ipc_panic!(
                    "ipc: server {} sent malformed error reply ({} bytes)",
                    server,
                    len,
                );
            };
            Err(err)
        }
        tag => crate::__ipc_panic!(
            "ipc: server {} sent invalid result tag {}",
            server,
            tag,
        ),
    }
}

// ---------------------------------------------------------------------------
// IpcCall builder
// ---------------------------------------------------------------------------

/// Maximum number of leases per IPC call.
const MAX_LEASES: usize = 4;

/// Builder for an outgoing IPC call.
///
/// Constructed by codegen, not by user code. The flow is:
/// 1. `IpcCall::new(target, kind, method)`
/// 2. `.push_arg(&val)` — serialize arguments into the static IPC buffer
/// 3. `.add_read_lease(data)` / `.add_write_lease(data)` — attach leases
/// 4. `.send_raw()` or `.send_void()` — execute; reply stays in `ipc_buf()`
pub struct IpcCall<'a> {
    target: kern::TaskId,
    opcode: u16,
    arglen: usize,
    leases: [Option<kern::Lease<'a>>; MAX_LEASES],
    lease_count: usize,
}

impl<'a> IpcCall<'a> {
    #[inline]
    pub fn new(target: kern::TaskId, kind: u8, method: u8) -> Self {
        Self {
            target,
            opcode: crate::opcode(kind, method),
            arglen: 0,
            leases: [None, None, None, None],
            lease_count: 0,
        }
    }

    #[inline]
    pub fn push_arg<T: serde::Serialize>(&mut self, val: &T) {
        let full = unsafe { &mut *ipc_buf() };
        let written = match crate::__postcard::to_slice(val, &mut full[self.arglen..]) {
            Ok(s) => s,
            Err(_) => crate::__ipc_panic!("ipc: argbuf overflow"),
        };
        self.arglen += written.len();
    }

    #[inline]
    pub fn set_raw_args(&mut self, data: &[u8]) {
        let full = unsafe { &mut *ipc_buf() };
        full[..data.len()].copy_from_slice(data);
        self.arglen = data.len();
    }

    #[inline]
    pub fn add_read_lease(&mut self, data: &'a [u8]) {
        debug_assert!(self.lease_count < MAX_LEASES);
        self.leases[self.lease_count] = Some(kern::Lease::read_only(data));
        self.lease_count += 1;
    }

    #[inline]
    pub fn add_write_lease(&mut self, data: &'a mut [u8]) {
        debug_assert!(self.lease_count < MAX_LEASES);
        self.leases[self.lease_count] = Some(kern::Lease::read_write(data));
        self.lease_count += 1;
    }

    /// Execute the IPC call. Reply data remains in `ipc_buf()`.
    #[inline]
    pub fn send_raw(
        self,
    ) -> Result<(kern::ResponseCode, usize), kern::TaskDeath> {
        let target = self.target;
        let opcode = self.opcode;
        let arglen = self.arglen;
        let lease_count = self.lease_count;
        let mut lease_arr = into_lease_array(self.leases);
        let buf = unsafe { &mut *ipc_buf() };
        let (rc, len) = kern::sys_send(
            target,
            opcode,
            buf,
            arglen,
            &mut lease_arr[..lease_count],
        )?;
        Ok((rc, len))
    }

    /// Execute the IPC call, ignoring the response body.
    #[inline]
    pub fn send_void(self) -> Result<kern::ResponseCode, kern::TaskDeath> {
        let target = self.target;
        let opcode = self.opcode;
        let arglen = self.arglen;
        let lease_count = self.lease_count;
        let mut lease_arr = into_lease_array(self.leases);
        let buf = unsafe { &mut *ipc_buf() };
        let (rc, _) = kern::sys_send(
            target,
            opcode,
            buf,
            arglen,
            &mut lease_arr[..lease_count],
        )?;
        Ok(rc)
    }
}

fn into_lease_array(leases: [Option<kern::Lease>; MAX_LEASES]) -> [kern::Lease; MAX_LEASES] {
    static EMPTY: &[u8] = &[];
    let [l0, l1, l2, l3] = leases;
    [
        l0.unwrap_or_else(|| kern::Lease::no_access(EMPTY)),
        l1.unwrap_or_else(|| kern::Lease::no_access(EMPTY)),
        l2.unwrap_or_else(|| kern::Lease::no_access(EMPTY)),
        l3.unwrap_or_else(|| kern::Lease::no_access(EMPTY)),
    ]
}
