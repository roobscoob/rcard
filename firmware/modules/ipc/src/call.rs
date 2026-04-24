//! Type-safe client-side IPC call builder.
//!
//! The codegen constructs an `IpcCall`, adds arguments and leases via
//! typed methods, and calls `.send()` to execute the IPC round-trip.
//! The builder handles serialization, the kernel send, and response
//! deserialization. Codegen never touches `sys_send` directly.

use crate::kern;

/// One-shot client IPC call. Consolidates `sys_send` + rc-checks + dead-gen
/// update into a single concrete fn so the macros don't paste this stanza
/// into every generated method body.
///
/// On `TaskDeath`, refreshes `server_id` with the kernel-supplied new
/// generation and returns `Err(Error::ServerDied)` so the caller's normal
/// `from_wire` mapping kicks in. On `ACCESS_VIOLATION` or any non-`SUCCESS`
/// response code, panics via `__ipc_panic!` — these are programming errors
/// (task ACL misconfigured, malformed opcode), not recoverable conditions.
///
/// Returns the number of bytes written into `retbuf` on success.
pub fn call_send(
    server_id: &'static crate::StaticTaskId,
    target: kern::TaskId,
    opcode: u16,
    argbuf: &[u8],
    retbuf: &mut [u8],
    leases: &mut [kern::Lease<'_>],
) -> Result<usize, crate::Error> {
    let (rc, len) = kern::sys_send(target, opcode, argbuf, retbuf, leases).map_err(|dead| {
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

/// Maximum number of leases per IPC call.
const MAX_LEASES: usize = 4;

/// Builder for an outgoing IPC call.
///
/// Constructed by codegen, not by user code. The flow is:
/// 1. `IpcCall::new(target, kind, method)`
/// 2. `.set_args(&args)` — serialize arguments
/// 3. `.add_read_lease(data)` / `.add_write_lease(data)` — attach leases
/// 4. `.send::<R>()` — execute and deserialize response
pub struct IpcCall<'a> {
    target: kern::TaskId,
    opcode: u16,
    argbuf: [u8; crate::HUBRIS_MESSAGE_SIZE_LIMIT],
    arglen: usize,
    leases: [Option<kern::Lease<'a>>; MAX_LEASES],
    lease_count: usize,
}

impl<'a> IpcCall<'a> {
    /// Create a new IPC call targeting `target` with the given kind and method.
    #[inline]
    pub fn new(target: kern::TaskId, kind: u8, method: u8) -> Self {
        Self {
            target,
            opcode: crate::opcode(kind, method),
            argbuf: [0u8; crate::HUBRIS_MESSAGE_SIZE_LIMIT],
            arglen: 0,
            leases: [None, None, None, None],
            lease_count: 0,
        }
    }

    /// Serialize a postcard-serializable value into the argument buffer
    /// at the current offset. Matches the wire format used by
    /// `gen_serialize_wire`, `arena.rs::dispatch_implicit`, etc.
    #[inline]
    pub fn push_arg<T: serde::Serialize>(&mut self, val: &T) {
        let written = crate::__postcard::to_slice(val, &mut self.argbuf[self.arglen..])
            .expect("ipc: IpcCall argbuf overflow");
        self.arglen += written.len();
    }

    /// Set the payload from a pre-serialized byte slice.
    #[inline]
    pub fn set_raw_args(&mut self, data: &[u8]) {
        self.argbuf[..data.len()].copy_from_slice(data);
        self.arglen = data.len();
    }

    /// Add a read-only lease (caller lends data for the server to read).
    #[inline]
    pub fn add_read_lease(&mut self, data: &'a [u8]) {
        assert!(
            self.lease_count < MAX_LEASES,
            "ipc: too many leases (max {})",
            MAX_LEASES
        );
        self.leases[self.lease_count] = Some(kern::Lease::read_only(data));
        self.lease_count += 1;
    }

    /// Add a write lease (caller lends a buffer for the server to write into).
    #[inline]
    pub fn add_write_lease(&mut self, data: &'a mut [u8]) {
        assert!(
            self.lease_count < MAX_LEASES,
            "ipc: too many leases (max {})",
            MAX_LEASES
        );
        self.leases[self.lease_count] = Some(kern::Lease::read_write(data));
        self.lease_count += 1;
    }

    /// Execute the IPC call, returning raw response bytes.
    ///
    /// Codegen deserializes the response inline using zerocopy.
    #[inline]
    pub fn send_raw(
        self,
    ) -> Result<
        (
            kern::ResponseCode,
            usize,
            [u8; crate::HUBRIS_MESSAGE_SIZE_LIMIT],
        ),
        kern::TaskDeath,
    > {
        let mut retbuf = [0u8; crate::HUBRIS_MESSAGE_SIZE_LIMIT];
        let target = self.target;
        let opcode = self.opcode;
        let arglen = self.arglen;
        let lease_count = self.lease_count;
        let argbuf = self.argbuf;
        let mut lease_arr = into_lease_array(self.leases);
        let (rc, len) = kern::sys_send(
            target,
            opcode,
            &argbuf[..arglen],
            &mut retbuf,
            &mut lease_arr[..lease_count],
        )?;
        Ok((rc, len, retbuf))
    }

    /// Execute the IPC call, ignoring the response body.
    ///
    /// Used for fire-and-forget calls (e.g., implicit destroy).
    #[inline]
    pub fn send_void(self) -> Result<kern::ResponseCode, kern::TaskDeath> {
        let mut retbuf = [0u8; 0];
        let target = self.target;
        let opcode = self.opcode;
        let arglen = self.arglen;
        let lease_count = self.lease_count;
        let argbuf = self.argbuf;
        let mut lease_arr = into_lease_array(self.leases);
        let (rc, _) = kern::sys_send(
            target,
            opcode,
            &argbuf[..arglen],
            &mut retbuf,
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
