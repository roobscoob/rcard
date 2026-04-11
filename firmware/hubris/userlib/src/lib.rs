//! User support library for Hubris.
//!
//! This crate provides Rust wrappers for most kernel interfaces, with a
//! particular focus on syscalls. Every Hubris task depends on this crate (or a
//! similar one implementing the same stuff), so it's designed to be fairly
//! minimal. In particular, `userlib` is designed to not depend on any
//! application configuration state.
//!
//! Routines in this crate are also designed to avoid `panic!` where possible.
//!
//! # Syscalls
//!
//! Syscalls are represented by Rust functions whose names begin with `sys_`.
//! Some Hubris kernel syscalls are represented here by multiple functions to
//! make things easier --- for example, the multi-function configurable syscall
//! [`sys_recv`] can be used more easily as [`sys_recv_open`] and
//! [`sys_recv_notification`].
//!
//! Syscalls are being added to this crate as they get used, so some unusual
//! syscalls defined in the Hubris ABI may be missing. This is likely not a
//! deliberate design choice, so, patches welcome.
//!
//! The full set of implemented syscalls and variants is:
//!
//! - [`sys_borrow_info`]
//! - [`sys_borrow_read`]
//! - [`sys_borrow_write`]
//! - [`sys_enable_irq`]
//!   - Variant: [`sys_enable_irq_and_clear_pending`]
//! - [`sys_post`]
//! - [`sys_panic`]
//! - [`sys_recv`]
//!   - Variant: [`sys_recv_msg`]
//!   - Variant: [`sys_recv_open`]
//!   - Variant: [`sys_recv_msg_open`]
//!   - Variant: [`sys_recv_notification`]
//! - [`sys_refresh_task_id`]
//! - [`sys_reply`]
//! - [`sys_reply_fault`]
//! - [`sys_send`]
//!   - Variant: [`sys_send_to_kernel`]
//!   - Wrapper: [`send_with_retry_on_death`]
//!
//! # Panic support
//!
//! `userlib` provides the Rust `panic_handler` that allows tasks to panic. This
//! is controlled by two features, which should only be set in top-level task
//! executables (and not in libraries):
//!
//! - `no-panic-messages` suppresses panic message generation, replacing any
//!   panic message with the fixed string `"PANIC"`. This can save considerable
//!   flash space in a task, at the cost of making debugging annoying.
//!
//! - `no-panic` converts any `panic!` that is not successfully optimized away
//!   by the compiler into an **error at link time.** This can be used to
//!   prevent panics from creeping into tasks that don't want them. Currently,
//!   _locating_ the panic in question is kind of a pain.
//!
//! If you provide both features, `no-panic` takes precedence.
//!
//! # Startup code
//!
//! This crate provides the initial `_start` routine responsible for setting up
//! the environment for Rust code in your task. It is currently not configurable
//! and should appear as if by magic.
//!
//! # Missing from this crate
//!
//! In the interest of keeping this crate minimal, some APIs you might expect
//! are missing.
//!
//! - KIPCs: since they're expected to only be used by the supervisor task,
//!   they're in the separate `kipc` crate.
//! - Named notification bit access: see the `hubris-notifications` crate.
//! - Task introspection, task count: see the `hubris-num-tasks` crate.
//! - Task slots, accessing tasks by name: see the `hubris-task-slots` crate.

#![no_std]

use core::cell::Cell;
use core::marker::PhantomData;

/// Syscall numbers, defined in the Hubris kernel ABI.
pub enum Sysnum {
    Send = 0,
    Recv = 1,
    Reply = 2,
    SetTimer = 3,
    BorrowRead = 4,
    BorrowWrite = 5,
    BorrowInfo = 6,
    IrqControl = 7,
    Panic = 8,
    GetTimer = 9,
    RefreshTaskId = 10,
    Post = 11,
    ReplyFault = 12,
}

/// Number of (high order) bits in a task ID that are used to indicate
/// generation.
pub const GEN_BITS: u32 = 6;

/// A handle to an instance of a task.
///
/// `TaskId`s consist of two parts, a _task index_ and a _generation number._
/// The task index permanently designates a task in the application, while the
/// generation number is revved each time the task restarts.
///
/// We use the combination of both index and generation when interacting with
/// other tasks, to make it easier to detect when a peer has restarted during
/// the interaction. Generally, an operation applied to the wrong `TaskId` will
/// produce a `TaskDeath` error, which contains the peer's _new_ generation in
/// case you'd like to update your `TaskId` (by calling
/// [`TaskId::with_generation`].
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct TaskId(u16);

impl TaskId {
    /// The fake `TaskId` assigned to the kernel in the Hubris ABI.
    ///
    /// This is used as a "sender" for "messages" that are really kernel
    /// notifications, as well as a destination for the KIPC interface (see the
    /// `kipc` crate for details).
    pub const KERNEL: TaskId = Self(!0);

    /// Creates a `TaskId` binary representation from an index and generation.
    ///
    /// If you find yourself passing a generation of zero, see [`TaskId::gen0`].
    ///
    /// Note that this can produce a `TaskId` that is not legal in the current
    /// application, such as one with a really large bogus index. This is fine
    /// until you use it. To reduce this risk, see the `hubris_task_slots` crate
    /// for a way to avoid hardcoding task indices in your code, or (if they
    /// really must be dynamic) the `hubris_num_tasks` crate for validating
    /// them.
    pub const fn new(task_index: u16, generation: Gen) -> Self {
        Self(task_index & ((1 << (16 - GEN_BITS)) - 1)
            | ((generation.0 as u16) << (16 - GEN_BITS)))
    }

    /// Produces a `TaskId` for the given index, with generation zero.
    ///
    /// This is usually a good place to start; on the first use, the kernel will
    /// hand back the _correct_ generation if it's wrong.
    pub const fn gen0(task_index: u16) -> Self {
        Self::new(task_index, Gen::DEFAULT)
    }

    /// Derives a new `TaskId` with the same index as `self` but an altered
    /// `generation`.
    pub const fn with_generation(self, generation: Gen) -> Self {
        Self::new(self.task_index(), generation)
    }

    /// Extracts the task index from this `TaskId`.
    pub const fn task_index(self) -> u16 {
        self.0 & ((1 << (16 - GEN_BITS)) - 1)
    }

    /// Extracts the generation from this `TaskId`.
    pub const fn generation(self) -> Gen {
        Gen((self.0 >> (16 - GEN_BITS)) as u8)
    }
}

/// In case you need to create a `TaskId` from its bitwise representation in the
/// Hubris ABI, here you go.
impl From<TaskId> for u16 {
    fn from(tid: TaskId) -> u16 {
        tid.0
    }
}

impl From<u16> for TaskId {
    fn from(raw: u16) -> TaskId {
        TaskId(raw)
    }
}

impl core::fmt::Display for TaskId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}", self.task_index(), self.generation().0)
    }
}

/// A generation number for a task.
///
/// This represents the generation number used in IPCs and `TaskId`s, which is a
/// truncation of the kernel's true generation number for a task. The generation
/// number is [`GEN_BITS`] long.
///
/// Currently, this type is opaque and is only useful for extracting from
/// `TaskId`s and comparing, but we could change this if desired.
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq, Default)]
pub struct Gen(u8);

impl Gen {
    /// The starting generation for tasks, in `const` form.
    pub const DEFAULT: Self = Self(0);
}

/// A reference to memory being loaned to the recipient of an IPC.
///
/// IPCs generated using the `sys_send` function take a table of leases. For the
/// duration of the send, the recipient has access to the caller's memory
/// described by those leases.
///
/// Each lease refers to a specific range of bytes in the address space, plus
/// some attributes (currently just read and write flags).
///
/// Normally, an IPC code generator will do this for you, but if you'd like to
/// do it manually: create each `Lease` using [`Lease::read_only`],
/// [`Lease::read_write`], or [`Lease::write_only`], and place them in an array
/// for `sys_send`.
///
/// Since a `Lease` borrows the memory in the Rust sense of the term "borrow,"
/// the task that creates a `Lease` must drop it before it can access the loaned
/// memory again, at least for writable leases.
#[repr(transparent)]
pub struct Lease<'a> {
    inner: AbiLease,
    _marker: PhantomData<&'a mut ()>,
}

impl<'a> Lease<'a> {
    /// Creates a `Lease` that gives its recipient the ability to read `data`.
    pub fn read_only(data: &[u8]) -> Self {
        Self {
            inner: AbiLease {
                attributes: LeaseAttributes::READ,
                base_address: data.as_ptr(),
                length: data.len(),
            },
            _marker: PhantomData,
        }
    }

    /// Creates a `Lease` that gives its recipient the ability to read and write
    /// `data`.
    pub fn read_write(data: &mut [u8]) -> Self {
        Self {
            inner: AbiLease {
                attributes: LeaseAttributes::READ | LeaseAttributes::WRITE,
                base_address: data.as_mut_ptr(),
                length: data.len(),
            },
            _marker: PhantomData,
        }
    }

    /// Creates a `Lease` that gives its recipient the ability to write `data`,
    /// but not read it.
    pub fn write_only(data: &mut [u8]) -> Self {
        Self {
            inner: AbiLease {
                attributes: LeaseAttributes::WRITE,
                base_address: data.as_mut_ptr(),
                length: data.len(),
            },
            _marker: PhantomData,
        }
    }

    /// Creates a lease that borrows `data` but grants no access to it. This is
    /// weird and rarely useful, but it's included for symmetry and to make the
    /// code generator easier.
    pub fn no_access(data: &[u8]) -> Self {
        Self {
            inner: AbiLease {
                attributes: LeaseAttributes::empty(),
                base_address: data.as_ptr(),
                length: data.len(),
            },
            _marker: PhantomData,
        }
    }
}

bitflags::bitflags! {
    /// Collection of attribute flags for a `Lease`.
    #[derive(Copy, Clone, Debug)]
    #[repr(transparent)]
    pub struct LeaseAttributes: u32 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;

        const _ = !0;
    }
}

/// Internal `Lease` memory layout, defined by the Hubris kernel ABI.
#[repr(C)]
struct AbiLease {
    /// Attributes of the lease.
    attributes: LeaseAttributes,
    /// Starting address of memory (modeled as a `*const` here though the memory
    /// may be held mutably).
    base_address: *const u8,
    /// Number of bytes in the lease.
    length: usize,
}

/// Response code returned from most syscalls.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ResponseCode(pub u32);

impl ResponseCode {
    /// The `ResponseCode` used to indicate success.
    pub const SUCCESS: Self = Self(0);
}

impl From<ResponseCode> for u32 {
    fn from(value: ResponseCode) -> Self {
        value.0
    }
}

/// Error used in syscalls to indicate that the peer has restarted since you
/// last checked.
///
/// You can update your record of the peer's generation using
/// [`TaskDeath::new_generation`].
#[derive(Copy, Clone, Debug)]
pub struct TaskDeath {
    now: Gen,
}

impl TaskDeath {
    pub fn new_generation(self) -> Gen {
        self.now
    }
}

/// Error used in the `TryFrom<ResponseCode>` impl for `TaskDeath`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NotADeadCode;

impl TryFrom<ResponseCode> for TaskDeath {
    type Error = NotADeadCode;
    fn try_from(rc: ResponseCode) -> Result<Self, Self::Error> {
        if rc.0 & 0xFFFF_FF00 == 0xFFFF_FF00 {
            Ok(TaskDeath { now: Gen(rc.0 as u8) })
        } else {
            Err(NotADeadCode)
        }
    }
}

pub enum MessageOrNotification<'a> {
    Message(Message<'a>),
    Notification(u32),
}

/// Information about a received message.
#[derive(Debug)]
pub struct Message<'a> {
    /// The ID of the task that sent this message. This will not be
    /// [`TaskId::KERNEL`] unless you fabricate it yourself.
    pub sender: TaskId,
    /// The operation code the sender requests.
    pub operation: u16,
    /// A reference to the subset of the buffer passed to `sys_recv` that
    /// contains the sender's message, if it fit in the buffer. If the sender's
    /// message was too long, this is `Err(Truncated)` instead.
    pub data: Result<&'a [u8], Truncated>,
    /// The number of bytes the sender has alloted to receive our response.
    pub reply_capacity: usize,
    /// The number of leases the sender has furnished. (Note that this tells us
    /// nothing about the access the leases actually permit; for that, see the
    /// [`sys_borrow_info`] syscall.
    pub lease_count: usize,
}

/// Error type used with [`Message`] to indicate that a message wouldn't fit
/// in our buffer.
#[derive(Copy, Clone, Debug)]
pub struct Truncated;

/// Information about a task's timer.
#[derive(Copy, Clone, Debug)]
pub struct TimerSettings {
    /// Current timestamp, in kernel ticks since restart.
    pub now: u64,
    /// Task's alarm, if configured. If the alarm is set, this will contain
    /// `Some((deadline, notification))`, indicating that at kernel time
    /// `deadline` the kernel will post `notification` to the task.
    pub alarm: Option<(u64, u32)>,
}

/// Information about borrowed memory, provided by a lease from a sending task.
#[derive(Copy, Clone, Debug)]
pub struct BorrowInfo {
    /// Our access to the memory.
    pub atts: LeaseAttributes,
    /// Number of bytes available.
    pub len: usize,
}

/// Error type used in IPC servers to trigger the [`sys_reply_fault`] syscall,
/// inducing a panic in a client.
#[derive(Copy, Clone, Debug)]
pub enum ReplyFaultReason {
    /// The message indicated some operation number that is unknown to the
    /// server -- which almost certainly indicates that the client intended the
    /// message for a different kind of server.
    UndefinedOperation = 0,
    /// The message sent by the client had the wrong size to even attempt
    /// parsing by the server -- either too short or too long. (Because most
    /// messages are fixed size, it currently doesn't seem useful to distinguish
    /// between too-short and too-long.)
    BadMessageSize = 1,
    /// The server attempted to parse the message, and couldn't. This may
    /// indicate an enum with an illegal value, or a more nuanced error on
    /// operations that use serde encoding.
    BadMessageContents = 2,
    /// The client did not provide the leases required for the operation, or
    /// provided them with the wrong attributes.
    BadLeases = 3,
    /// The client did not provide a reply buffer large enough to receive the
    /// server's reply, despite this information being implied by the IPC
    /// protocol.
    ReplyBufferTooSmall = 4,

    /// Application-defined: The client attempted to operate on a resource that
    /// is not available to them due to mandatory access control or other type
    /// of access validation.
    AccessViolation = 5,
}


/// Requests that the CPU spin waiting for an interrupt, if possible.
///
/// Generally, this function is only useful to one task in the system, the idle
/// task. But it's included here to make portability easier.
///
/// Note that this is _not_ a yield operation! On most processors, this function
/// will starve any lower-priority tasks until an interrupt arrives. This
/// behavior can be useful for reducing the response latency to such an
/// interrupt, but should be used with caution. Treat it like a `loop {}`.
pub fn idle() {
    arch::idle();
}


///////////////////////////////////////////////////////////////////////////////
// Architecture-specific includes and re-exports

#[path = "arch/arm_m.rs"]
mod arch;

#[doc(inline)]
pub use self::arch::sys_panic;

#[doc(inline)]
pub use self::arch::sys_send;

#[doc(inline)]
pub use self::arch::sys_send_to_kernel;

#[doc(inline)]
pub use self::arch::sys_recv;

#[doc(inline)]
pub use self::arch::sys_recv_msg;

#[doc(inline)]
pub use self::arch::sys_recv_open;

#[doc(inline)]
pub use self::arch::sys_recv_msg_open;

#[doc(inline)]
pub use self::arch::sys_recv_notification;

#[doc(inline)]
pub use self::arch::sys_reply;

#[doc(inline)]
pub use self::arch::sys_reply_fault;

#[doc(inline)]
pub use self::arch::sys_set_timer;

#[doc(inline)]
pub use self::arch::sys_get_timer;

#[doc(inline)]
pub use self::arch::sys_enable_irq;

#[doc(inline)]
pub use self::arch::sys_enable_irq_and_clear_pending;

#[doc(inline)]
pub use self::arch::sys_borrow_info;

#[doc(inline)]
pub use self::arch::sys_borrow_read;

#[doc(inline)]
pub use self::arch::sys_borrow_write;

#[doc(inline)]
pub use self::arch::sys_post;

#[doc(inline)]
pub use self::arch::sys_refresh_task_id;

/// Block the current task indefinitely by receiving into an empty buffer
/// with no notification mask. Used by stub/idle tasks.
pub fn sys_recv_stub() {
    let _ = sys_recv_open(&mut [], 0);
}

///////////////////////////////////////////////////////////////////////////////
// Convenience wrappers around raw syscalls.

/// Utility function for sending an IPC with automatic retry if the server has
/// restarted.
///
/// This is intended for the common case where a server has either _not_
/// restarted, or is restarting infrequently. In that case, the appropriate
/// response is to use the dead code returned from `sys_send` to update our
/// record of the `TaskId`, and send the message again.
///
/// This function has no sleep, exponential backoff, or other such techniques
/// for avoiding using 100% of the CPU if a server is crashlooping rapidly. This
/// is because there's no single choice that's right for every application. If
/// you need something fancier, you'll have to implement it using your knowledge
/// of your application environment.
pub fn send_with_retry_on_death(
    tid_holder: &Cell<TaskId>,
    operation: u16,
    outgoing: &[u8],
    incoming: &mut [u8],
    leases: &mut [Lease<'_>],
) -> (ResponseCode, usize) {
    loop {
        let r = sys_send(
            tid_holder.get(),
            operation,
            outgoing,
            incoming,
            leases,
        );
        match r {
            Ok(rc_and_len) => break rc_and_len,
            Err(dead) => {
                tid_holder.set(
                    tid_holder.get().with_generation(dead.new_generation())
                );
            }
        }
    }
}


// No panic handler — each task provides its own.

///////////////////////////////////////////////////////////////////////////////
// Hubris ABI version stamping for compat checking.

include!(concat!(env!("OUT_DIR"), "/hubris_abi_version.rs"));
