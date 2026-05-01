// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Kernel ABI definitions, shared between kernel and applications.

#![no_std]
#![forbid(clippy::wildcard_imports)]

use serde::{Deserialize, Serialize};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Names a particular incarnation of a task.
///
/// A `TaskId` combines two fields, a task index (which can be predicted at
/// compile time) and a task generation number. The generation number begins
/// counting at zero and wraps on overflow. Critically, the generation number of
/// a task is incremented when it is restarted. Attempts to correspond with a
/// task using an outdated generation number will return `DEAD`. This helps
/// provide assurance that your peer has not lost its memory between steps of a
/// multi-step IPC sequence.
///
/// If the IPC can be retried against a fresh instance of the peer, it's
/// reasonable to simply increment the generation number and try again, using
/// `TaskId::next_generation`.
///
/// The task index is in the lower `TaskId::INDEX_BITS` bits, while the
/// generation is in the remaining top bits.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskId(pub u16);

impl TaskId {
    /// The all-ones `TaskId` is reserved to represent the "virtual kernel
    /// task."
    pub const KERNEL: Self = Self(!0);

    /// Reserved TaskId for an unbound userlib::task_slot!()
    pub const UNBOUND: Self = Self(Self::INDEX_MASK - 1);

    /// Number of bits in a `TaskId` used to represent task index, rather than
    /// generation number. This must currently be 15 or smaller.
    pub const INDEX_BITS: u32 = 10;

    /// Derived mask of the index bits portion.
    pub const INDEX_MASK: u16 = (1 << Self::INDEX_BITS) - 1;

    /// Fabricates a `TaskId` for a known index and generation number.
    pub const fn for_index_and_gen(index: usize, gen: Generation) -> Self {
        TaskId((index as u16 & Self::INDEX_MASK) | (gen.0 as u16) << Self::INDEX_BITS)
    }

    /// Extracts the index part of this ID.
    pub fn index(&self) -> usize {
        usize::from(self.0 & Self::INDEX_MASK)
    }

    /// Extracts the generation part of this ID.
    pub fn generation(&self) -> Generation {
        Generation((self.0 >> Self::INDEX_BITS) as u8)
    }

    pub fn next_generation(self) -> Self {
        Self::for_index_and_gen(self.index(), self.generation().next())
    }
}

impl core::fmt::Display for TaskId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}", self.index(), self.generation().0)
    }
}

/// Type used to track generation numbers.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
#[repr(transparent)]
pub struct Generation(u8);

impl Generation {
    pub const ZERO: Self = Self(0);

    pub fn next(self) -> Self {
        const MASK: u16 = 0xFFFF << TaskId::INDEX_BITS >> TaskId::INDEX_BITS;
        Generation(self.0.wrapping_add(1) & MASK as u8)
    }
}

impl From<u8> for Generation {
    fn from(x: u8) -> Self {
        Self(x)
    }
}

impl From<Generation> for u8 {
    fn from(x: Generation) -> Self {
        x.0
    }
}

/// Newtype wrapper for an interrupt index
#[derive(
    Copy,
    Clone,
    Debug,
    FromBytes,
    Immutable,
    KnownLayout,
    Serialize,
    Deserialize,
    Hash,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
)]
#[repr(transparent)]
pub struct InterruptNum(pub u32);
impl phash::PerfectHash for InterruptNum {
    fn phash(&self, v: u32) -> usize {
        self.0.wrapping_mul(v) as usize
    }
}
impl InterruptNum {
    pub const fn invalid() -> Self {
        Self(u32::MAX)
    }
    pub fn is_valid(&self) -> bool {
        self.0 != u32::MAX
    }
}

/// Struct containing the task which waits for an interrupt, and the expected
/// notification mask associated with the IRQ.
#[derive(
    Copy,
    Clone,
    Debug,
    FromBytes,
    Immutable,
    KnownLayout,
    Serialize,
    Deserialize,
    Hash,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
)]
pub struct InterruptOwner {
    /// Which task to notify, by index.
    pub task: u32,
    /// Which notification bits to set.
    pub notification: u32,
}
impl phash::PerfectHash for InterruptOwner {
    fn phash(&self, v: u32) -> usize {
        self.task
            .wrapping_mul(v)
            .wrapping_add(self.notification.wrapping_mul(!v)) as usize
    }
}
impl InterruptOwner {
    pub const fn invalid() -> Self {
        Self {
            task: u32::MAX,
            notification: 0,
        }
    }
    pub fn is_valid(&self) -> bool {
        !(self.task == u32::MAX && self.notification == 0)
    }
}

/// Description of one interrupt response.
#[derive(Clone, Debug, FromBytes, Immutable, KnownLayout, Serialize, Deserialize)]
pub struct Interrupt {
    /// Which interrupt number is being hooked.
    pub irq: InterruptNum,
    /// The owner of this interrupt.
    pub owner: InterruptOwner,
}

/// Structure describing a lease in task memory.
///
/// At SEND, the task gives us the base and length of a section of memory that
/// it *claims* contains structs of this type.
#[derive(Copy, Clone, Debug, FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct ULease {
    /// Lease attributes.
    pub attributes: LeaseAttributes,
    /// Base address of leased memory. This is equivalent to the base address
    /// field in `USlice`, but isn't represented as a `USlice` because we leave
    /// the internal memory representation of `USlice` out of the ABI.
    pub base_address: u32,
    /// Length of leased memory, in bytes.
    pub length: u32,
}

#[derive(Copy, Clone, Debug, FromBytes, Immutable, KnownLayout, PartialEq, Eq)]
#[repr(transparent)]
pub struct LeaseAttributes(u32);

bitflags::bitflags! {
    impl LeaseAttributes: u32 {
        /// Allow the borrower to read this memory.
        const READ = 1 << 0;
        /// Allow the borrower to write this memory.
        const WRITE = 1 << 1;
    }
}

pub const FIRST_DEAD_CODE: u32 = 0xffff_ff00;

/// Response code returned by the kernel if the peer died or was restarted.
///
/// This always has the top 24 bits set to 1, with the `generation` in the
/// bottom 8 bits.
pub const fn dead_response_code(new_generation: Generation) -> u32 {
    FIRST_DEAD_CODE | new_generation.0 as u32
}

/// Utility for checking whether a code indicates that the peer was restarted
/// and extracting the generation if it is.
pub const fn extract_new_generation(code: u32) -> Option<Generation> {
    if (code & FIRST_DEAD_CODE) == FIRST_DEAD_CODE {
        Some(Generation(code as u8))
    } else {
        None
    }
}

/// Response code returned by the kernel if a lender has defected.
pub const DEFECT: u32 = 1;

/// Response code returned by the kernel if a borrow target is in a
/// hibernated memory region.
pub const HIBERNATED: u32 = 2;

/// A bitfield over suspension region indices. Bit 0 corresponds to
/// suspension region 0, bit 1 to region 1, and so on up to bit 31.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[repr(transparent)]
pub struct HibernatedRegionsBitfield(pub u32);

impl HibernatedRegionsBitfield {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn of_region(index: u32) -> Self {
        Self(1 << index)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn insert(&mut self, index: u32) {
        self.0 |= 1 << index;
    }

    pub const fn contains(self, index: u32) -> bool {
        (self.0 & (1 << index)) != 0
    }

    pub const fn union(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub const fn remove(&mut self, index: u32) {
        self.0 &= !(1 << index);
    }

    pub const fn take(&mut self, index: u32) -> bool {
        let mask = 1 << index;
        let had_bit = (self.0 & mask) != 0;
        self.0 &= !mask;
        had_bit
    }

    /// Returns an iterator that yields `&R` for each set bit, indexing
    /// into `slice`. Bits whose index is out of range are silently
    /// skipped. Yields from least significant to most significant bit.
    pub fn iter<'a, R>(self, slice: &'a [R]) -> BitSliceIter<'a, R> {
        BitSliceIter {
            mask: self.0,
            slice,
        }
    }

    /// Returns an iterator that yields `&mut R` for each set bit,
    /// indexing into `slice`. Bits whose index is out of range are
    /// silently skipped. Yields from least significant to most
    /// significant bit.
    pub fn iter_mut<'a, R>(self, slice: &'a mut [R]) -> BitSliceIterMut<'a, R> {
        BitSliceIterMut {
            mask: self.0,
            slice,
        }
    }
}

/// Iterator that yields `&R` for each set bit in ascending order.
pub struct BitSliceIter<'a, R> {
    mask: u32,
    slice: &'a [R],
}

impl<'a, R> BitSliceIter<'a, R> {
    pub fn empty() -> Self {
        Self {
            mask: 0,
            slice: &[],
        }
    }
}

impl<'a, R> Iterator for BitSliceIter<'a, R> {
    type Item = &'a R;

    fn next(&mut self) -> Option<&'a R> {
        loop {
            if self.mask == 0 {
                return None;
            }
            let i = self.mask.trailing_zeros() as usize;
            self.mask &= self.mask - 1;
            if let Some(item) = self.slice.get(i) {
                return Some(item);
            }
        }
    }
}

/// Iterator that yields `&mut R` for each set bit in ascending order.
pub struct BitSliceIterMut<'a, R> {
    mask: u32,
    slice: &'a mut [R],
}

impl<'a, R> Iterator for BitSliceIterMut<'a, R> {
    type Item = &'a mut R;

    fn next(&mut self) -> Option<&'a mut R> {
        loop {
            if self.mask == 0 {
                return None;
            }
            let i = self.mask.trailing_zeros() as usize;
            self.mask &= self.mask - 1;
            if i < self.slice.len() {
                // Safety: each bit index is yielded at most once (we
                // clear it), so we never alias the same element twice.
                let ptr = self.slice.as_mut_ptr();
                return Some(unsafe { &mut *ptr.add(i) });
            }
        }
    }
}

/// State used to make scheduling decisions.
#[derive(Copy, Clone, Debug, Deserialize, Serialize)]
pub enum TaskState {
    /// Task is healthy and can be scheduled subject to the `SchedState`
    /// requirements.
    Healthy(SchedState),
    /// Task has been stopped by a fault and must not be scheduled without
    /// intervention.
    Faulted {
        /// Information about the fault.
        fault: FaultInfo,
        /// Record of the previous healthy state at the time the fault was
        /// taken.
        original_state: SchedState,
    },
    /// Task is in a frozen state due either to its underlying memory being
    /// removed, or due to a debugger request.
    Suspended {
        /// Record the previous healthy state at the time the task was
        /// suspended.
        original_healthy_state: SchedState,

        /// Record the previous fault information if the task was
        /// faulted at the time of suspension.
        original_fault_info: Option<FaultInfo>,

        /// What's keeping this task suspended?
        hibernated_regions: HibernatedRegionsBitfield,
        debug_request: bool,
    },
}

impl TaskState {
    /// Checks if a task in this state is ready to accept a message sent by
    /// `caller`. This will return `true` if the state is an open receive, or a
    /// closed receive naming the caller specifically; otherwise, it will return
    /// `false`.
    pub fn can_accept_message_from(&self, caller: TaskId) -> bool {
        match self {
            TaskState::Healthy(SchedState::InRecv(None)) => true,
            TaskState::Healthy(SchedState::InRecv(Some(peer))) => *peer == caller,
            _ => false,
        }
    }

    /// Checks if a task in this state is trying to deliver a message to
    /// `target`.
    pub fn is_sending_to(&self, target: TaskId) -> bool {
        match self {
            TaskState::Healthy(SchedState::InSend(t)) => *t == target,
            _ => false,
        }
    }

    /// Checks if a task in this state can be unblocked with a notification.
    pub fn can_accept_notification(&self) -> bool {
        match self {
            TaskState::Healthy(SchedState::InRecv(_)) => true,
            TaskState::Suspended {
                original_fault_info: None,
                original_healthy_state: SchedState::InRecv(_),
                ..
            } => true,
            _ => false,
        }
    }
}

impl Default for TaskState {
    fn default() -> Self {
        TaskState::Healthy(SchedState::Stopped)
    }
}

/// Scheduler parameters for a healthy task.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum SchedState {
    /// This task is ignored for scheduling purposes.
    Stopped,
    /// This task could be scheduled on the CPU.
    Runnable,
    /// This task is blocked waiting to deliver a message to the given task.
    InSend(TaskId),
    /// This task is blocked waiting for a reply from the given task.
    InReply(TaskId),
    /// This task was replied to in a suspended state, and is blocked
    /// waiting for the hibernation region to be restored so the
    /// reply can be delivered.
    InSuspendedReply(TaskId),
    /// This task is blocked waiting for messages, either from any source
    /// (`None`) or from a particular sender only.
    InRecv(Option<TaskId>),
}

impl From<SchedState> for TaskState {
    fn from(s: SchedState) -> Self {
        Self::Healthy(s)
    }
}

/// A record describing a fault taken by a task.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum FaultInfo {
    /// The task has violated memory access rules. This may have come from a
    /// memory protection fault while executing the task (in the case of
    /// `source` `User`), from overflowing a stack, or from checks on kernel
    /// syscall arguments (`source` `Kernel`).
    MemoryAccess {
        /// Problematic address that the task accessed, or asked the kernel to
        /// access. This is `Option` because there are cases of processor
        /// protection faults that don't provide a precise address.
        address: Option<u32>,
        /// Origin of the fault.
        source: FaultSource,
    },
    /// A task has overflowed its stack. We can always determine the bad
    /// stack address, but we can't determine the PC
    StackOverflow { address: u32 },
    /// A task has induced a bus error
    BusError {
        address: Option<u32>,
        source: FaultSource,
    },
    /// Divide-by-zero
    DivideByZero,
    /// Attempt to execute non-executable memory
    IllegalText,
    /// Execution of an illegal instruction
    IllegalInstruction,
    /// Other invalid operation, with 32-bit code. We use this for faults that
    /// aren't general across architectures or may not have enough diagnosis
    /// information. The code is architecture-specific.
    ///
    /// - ARMv7/8-M: used for faults not otherwise enumerated in this type; the
    ///   code is the bits of the Configurable Fault Status Register.
    /// - ARMv6-M: used for all faults, as v6 doesn't distinguish faults. The
    ///   code is always 0.
    InvalidOperation(u32),
    /// Arguments passed to a syscall were invalid. TODO: this should become
    /// more descriptive, it's a placeholder.
    SyscallUsage(UsageError),
    /// A task has explicitly aborted itself with a panic.
    Panic,
    /// A fault has been injected into this task by another task
    Injected(TaskId),
    /// A fault has been delivered by a server task.
    FromServer(TaskId, ReplyFaultReason),
    /// A task lost its backing memory during hibernation.
    LostRegion {
        /// The regions that were lost, as a bitfield over region indices.
        regions: HibernatedRegionsBitfield,
    },
    /// You tried to read memory currently hibernated.
    HibernatedMemoryAccess {
        /// The address you tried to read.
        address: u32,
    },
}

/// We're using an explicit `TryFrom` impl for `Sysnum` instead of
/// `FromPrimitive` because the kernel doesn't currently depend on `num-traits`
/// and this seems okay.
impl core::convert::TryFrom<u32> for ReplyFaultReason {
    type Error = ();

    fn try_from(x: u32) -> Result<Self, Self::Error> {
        match x {
            0 => Ok(Self::UndefinedOperation),
            1 => Ok(Self::BadMessageSize),
            2 => Ok(Self::BadMessageContents),
            3 => Ok(Self::BadLeases),
            4 => Ok(Self::ReplyBufferTooSmall),
            5 => Ok(Self::AccessViolation),
            _ => Err(()),
        }
    }
}

impl From<UsageError> for FaultInfo {
    fn from(e: UsageError) -> Self {
        Self::SyscallUsage(e)
    }
}

/// A kernel-defined fault, arising from how a user task behaved.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum UsageError {
    /// A program used an undefined syscall number.
    BadSyscallNumber,
    /// A program specified a slice as a syscall argument, but the slice is
    /// patently invalid: it is either unaligned for its type, or it is
    /// expressed such that it would wrap around the end of the address space.
    /// Neither of these conditions is ever legal, so this represents a
    /// malfunction in the caller.
    InvalidSlice,
    /// A program named a task ID that will never be valid, as it's out of
    /// range.
    TaskOutOfRange,
    /// A program named a valid task ID, but attempted to perform an operation
    /// on it that is illegal or otherwise forbidden.
    IllegalTask,
    LeaseOutOfRange,
    OffsetOutOfRange,
    NoIrq,
    BadKernelMessage,
    BadReplyFaultReason,
    NotSupervisor,

    /// A server is attempting to reply with a message that is too large for the
    /// client to handle.
    ReplyTooBig,
}

/// Origin of a fault.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum FaultSource {
    /// User code did something that was intercepted by the processor.
    User,
    /// User code asked the kernel to do something bad on its behalf.
    Kernel,
}

/// Reasons a server might cite when using the `REPLY_FAULT` syscall.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
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

/// Enumeration of syscall numbers.
#[repr(u32)]
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
    IrqStatus = 13,
}

/// We're using an explicit `TryFrom` impl for `Sysnum` instead of
/// `FromPrimitive` because the kernel doesn't currently depend on `num-traits`
/// and this seems okay.
impl core::convert::TryFrom<u32> for Sysnum {
    type Error = ();

    fn try_from(x: u32) -> Result<Self, Self::Error> {
        match x {
            0 => Ok(Self::Send),
            1 => Ok(Self::Recv),
            2 => Ok(Self::Reply),
            3 => Ok(Self::SetTimer),
            4 => Ok(Self::BorrowRead),
            5 => Ok(Self::BorrowWrite),
            6 => Ok(Self::BorrowInfo),
            7 => Ok(Self::IrqControl),
            8 => Ok(Self::Panic),
            9 => Ok(Self::GetTimer),
            10 => Ok(Self::RefreshTaskId),
            11 => Ok(Self::Post),
            12 => Ok(Self::ReplyFault),
            13 => Ok(Self::IrqStatus),
            _ => Err(()),
        }
    }
}

/// A region to be dumped from a task
#[derive(Copy, Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct TaskDumpRegion {
    pub base: u32,
    pub size: u32,
}

/// Representation of kipc numbers
pub enum Kipcnum {
    ReadTaskStatus = 1,
    ReinitTask = 2,
    FaultTask = 3,
    ReadImageId = 4,
    Reset = 5,
    GetTaskDumpRegion = 6,
    ReadTaskDumpRegion = 7,
    SoftwareIrq = 8,
    FindFaultedTask = 9,
    ReadPanicMessage = 10,

    // "Extension" kipcnums, used by rose's fork
    HibernateRegion = 0x80 + 1,
    ReadHibernatedRegion = 0x80 + 2,
    WriteHibernatedRegion = 0x80 + 3,
    RestoreRegion = 0x80 + 4,

    SuspendTask = 0x80 + 5,
    RestoreTask = 0x80 + 6,
}

impl core::convert::TryFrom<u16> for Kipcnum {
    type Error = ();

    fn try_from(x: u16) -> Result<Self, Self::Error> {
        match x {
            1 => Ok(Self::ReadTaskStatus),
            2 => Ok(Self::ReinitTask),
            3 => Ok(Self::FaultTask),
            4 => Ok(Self::ReadImageId),
            5 => Ok(Self::Reset),
            6 => Ok(Self::GetTaskDumpRegion),
            7 => Ok(Self::ReadTaskDumpRegion),
            8 => Ok(Self::SoftwareIrq),
            9 => Ok(Self::FindFaultedTask),
            10 => Ok(Self::ReadPanicMessage),

            0x81 => Ok(Self::HibernateRegion),
            0x82 => Ok(Self::ReadHibernatedRegion),
            0x83 => Ok(Self::WriteHibernatedRegion),
            0x84 => Ok(Self::RestoreRegion),

            0x85 => Ok(Self::SuspendTask),
            0x86 => Ok(Self::RestoreTask),

            _ => Err(()),
        }
    }
}

pub const HEADER_MAGIC: u32 = 0x64_CE_D6_CA;
pub const CABOOSE_MAGIC: u32 = 0xCAB0_005E;

/// TODO: Add hash for integrity check
/// Later this will also be a signature block
#[repr(C)]
#[derive(Default, IntoBytes, FromBytes, KnownLayout, Immutable)]
pub struct ImageHeader {
    pub magic: u32,
    pub total_image_len: u32,
    pub _pad: [u32; 16], // previous location of SAU entries
    pub version: u32,
    pub epoch: u32,
}

// Corresponds to the ARM vector table, limited to what we need
// see ARMv8m B3.30 and B1.5.3 ARMv7m for the full description
#[repr(C)]
#[derive(Default, IntoBytes, KnownLayout, Immutable)]
pub struct ImageVectors {
    pub sp: u32,
    pub entry: u32,
}

bitflags::bitflags! {
    /// A set of bitflags representing the status of the interrupts mapped to a
    /// notification mask.
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct IrqStatus: u32 {
        /// If 1, this interrupt is enabled.
        const ENABLED = 1 << 0;
        /// If 1, an IRQ is currently pending for this interrupt.
        const PENDING = 1 << 1;
        ///If 1, a notification has been posted for this interrupt.
        const POSTED = 1 << 2;
    }
}

bitflags::bitflags! {
    /// Bitflags that can be passed into the `IRQ_CONTROL` syscall.
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct IrqControlArg: u32 {
        /// Enables the interrupt if present, disables if not present.
        const ENABLED = 1 << 0;
        /// If present, requests that any pending instance of this interrupt be
        // cleared.
        const CLEAR_PENDING = 1 << 1;
    }
}

/// Errors returned by [`Kipcnum::ReadPanicMessage`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u32)]
pub enum ReadPanicMessageError {
    /// The task in question has not panicked.
    TaskNotPanicked = 1,
    /// The task has panicked, but its panic message buffer is invalid, so the
    /// kernel has not let us have it.
    ///
    /// In practice, this is quite unlikely, and would require the task to have
    /// panicked with a panic message slice of a length that exceeds the end of
    /// the address space. Panicking via the Hubris userlib will never do this.
    /// But, since the panicked task could be any arbitrary binary...anything is
    /// possible.
    BadPanicBuffer = 2,
    /// The task has panicked, but its panic message buffer is currently
    /// hibernated, so we can't read it.
    PanicBufferHibernated = 3,
}

/// We're using an explicit `TryFrom` impl for `ReadPanicMessageError` instead of
/// `FromPrimitive` because the kernel doesn't currently depend on `num-traits`
/// and this seems okay.
impl core::convert::TryFrom<u32> for ReadPanicMessageError {
    type Error = ();

    fn try_from(x: u32) -> Result<Self, Self::Error> {
        match x {
            1 => Ok(Self::TaskNotPanicked),
            2 => Ok(Self::BadPanicBuffer),
            3 => Ok(Self::PanicBufferHibernated),
            _ => Err(()),
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[repr(transparent)]
pub struct HibernationEventId(pub u32);

impl HibernationEventId {
    /// Fabricates a `HibernationEventId` for a known index and generation number.
    pub fn from_parts(index: u8, generation: u32) -> Option<Self> {
        if index >= (1 << 5) {
            return None;
        }

        if generation >= (1 << 27) {
            return None;
        }

        Some(HibernationEventId((generation << 5) | index as u32))
    }

    /// The index is in the lower 5 bits of the ID, and identifies which
    /// hibernation event this is referring to.
    pub fn index(&self) -> u8 {
        (self.0 & 0x1F) as u8
    }

    /// The generation is in the upper 27 bits of the ID, and is incremented on each
    /// hibernation event for a given index.
    pub fn generation(&self) -> u32 {
        self.0 >> 5
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[repr(u32)]
pub enum RestoreMode {
    /// Restore the hibernated region, and attempt to restore the task to its
    /// original healthy state. If the task was faulted at the time of hibernation,
    /// attempt to restore the fault as well.
    Restored = 0,

    /// Fault the hibernated task with `FaultInfo::LostRegion`. This is intended to
    /// be used when the hibernated region contents cannot be restored,
    /// but a fault and reset of the task would allow it to function correctly.
    Lost = 1,
}

/// Errors returned by [`Kipcnum::ReinitTask`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u32)]
pub enum ReinitTaskError {
    /// The target task is currently suspended and cannot be reinitialized.
    TaskSuspended = 1,
}

/// Errors returned by [`Kipcnum::HibernateRegion`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u32)]
pub enum HibernateRegionMessageError {
    /// No predefined hibernation region matches the (base, size) pair.
    NoMatchingRegion = 1,

    /// The matching region is already hibernated.
    AlreadyHibernated = 2,

    /// The region's event ID generation space is exhausted.
    GenerationOverflow = 3,

    /// The region overlaps memory belonging to a protected task.
    ProtectedMemory = 4,
}

/// Errors returned by [`Kipcnum::ReadHibernatedRegion`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u32)]
pub enum ReadHibernatedRegionMessageError {
    /// The (address, length) specified by the caller is not hibernated.
    NotHibernated = 1,

    /// Missing a lease to write back the data.
    MissingWritebackLease = 2,

    /// Writeback lease is missing LeaseAttributes::WRITE.
    WritebackLeaseNotWritable = 3,
}

/// Errors returned by [`Kipcnum::WriteHibernatedRegion`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u32)]
pub enum WriteHibernatedRegionMessageError {
    /// The (address, length) specified by the caller is not hibernated.
    NotHibernated = 1,

    /// Missing a lease to read the source data from.
    MissingSourceLease = 2,

    /// Source lease is missing LeaseAttributes::READ.
    SourceLeaseNotReadable = 3,
}

/// Errors returned by [`Kipcnum::RestoreRegion`].
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u32)]
pub enum RestoreRegionMessageError {
    /// The event id is stale, out of range, or was never issued.
    InvalidEventId = 1,
}
