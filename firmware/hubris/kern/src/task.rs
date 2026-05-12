// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Implementation of tasks.

use core::ops::Range;

use abi::{
    FaultInfo, FaultSource, Generation, ReplyFaultReason, RestoreMode,
    SchedState, TaskId, TaskState, ULease, UsageError,
};
use zerocopy::{FromBytes, Immutable, KnownLayout};

use abi::{BitSliceIter, HibernatedRegionsBitfield};

use crate::descs::HibernationRegionDesc;
use crate::descs::{
    Priority, RegionAttributes, RegionDesc, TaskDesc, TaskFlags,
    REGIONS_PER_TASK,
};
use crate::err::UserError;
use crate::hibernate::SuspensionBuffer;
use crate::startup::{with_hibernation_regions, HUBRIS_FAULT_NOTIFICATION};
use crate::time::Timestamp;
use crate::umem::USlice;

/// Internal representation of a task.
///
/// The fields of this struct are private to this module so that we can maintain
/// some task invariants. These mostly have to do with ensuring that task
/// interactions remain consistent across state changes -- for example, setting
/// a task to RECV should process another task trying to SEND, if one exists.
#[repr(C)] // so location of SavedState is predictable
#[derive(Debug)]
pub struct Task {
    /// Saved machine state of the user program.
    save: crate::arch::SavedState,
    // NOTE: it is critical that the above field appear first!
    /// Current priority of the task.
    priority: Priority,
    /// State used to make status and scheduling decisions.
    state: TaskState,
    /// State for tracking the task's timer.
    timer: TimerState,
    /// Restart count for this task. We increment this whenever we reinitialize
    /// the task. The low bits of this become the task's generation number.
    generation: u32,

    /// Notification status.
    notifications: u32,

    /// Pointer to the ROM descriptor used to create this task, so it can be
    /// restarted.
    descriptor: &'static TaskDesc,

    /// Reply buffer used for suspension
    suspension_buffer: Option<SuspensionBuffer>,
}

impl Task {
    /// Creates a `Task` in its initial state, filling in fields from
    /// `descriptor`.
    pub fn from_descriptor(descriptor: &'static TaskDesc) -> Self {
        Task {
            priority: Priority(descriptor.priority),
            state: if descriptor.flags.contains(TaskFlags::START_AT_BOOT) {
                TaskState::Healthy(SchedState::Runnable)
            } else {
                TaskState::default()
            },

            descriptor,

            generation: 0,
            notifications: 0,
            save: crate::arch::SavedState::default(),
            timer: crate::task::TimerState::default(),

            suspension_buffer: None,
        }
    }

    /// Obtains access to the memory backing `slice` as a Rust slice, assuming
    /// that the task `self` can access it for read. This is used to access task
    /// memory from the kernel in validated form.
    ///
    /// This will treat memory marked `DEVICE` or `DMA` as inaccessible; see
    /// `assert_access` for more details.
    pub fn try_read<'a, T>(
        &'a self,
        slice: &'a USlice<T>,
    ) -> Result<&'a [T], FaultInfo>
    where
        T: FromBytes + Immutable + KnownLayout,
    {
        self.assert_access(
            slice,
            RegionAttributes::READ,
            RegionAttributes::DMA,
        )?;

        // Safety: assume_readable requires us to have validated that the
        // slice refers to normal task memory, which we did on the previous
        // line.
        unsafe { Ok(slice.assume_readable()) }
    }

    /// Obtains access to the memory backing `slice` as a Rust raw pointer
    /// range, if and only if the task `self` can access it for read. This is
    /// used to access task memory from the kernel in validated form.
    ///
    /// Because the result of this function is not a Rust slice, this can be
    /// used to interact with memory marked as `DMA` -- that is, normal memory
    /// that might be asynchronously modified (from the perspective of the CPU).
    /// If you want to access memory using a proper Rust slice, use `try_read`
    /// instead.
    ///
    /// Like `try_read` this will treat memory marked `DEVICE` as inaccessible;
    /// see `assert_access` for more details.
    pub fn try_read_dma<'a, T>(
        &'a self,
        slice: &'a USlice<T>,
    ) -> Result<Range<*const T>, FaultInfo>
    where
        T: FromBytes + Immutable + KnownLayout,
    {
        self.assert_access(
            slice,
            RegionAttributes::READ,
            RegionAttributes::empty(),
        )?;

        // Safety: assume_readable_raw requires us to have validated that
        // the slice refers to normal task memory, which we did on the
        // previous line.
        unsafe { Ok(slice.assume_readable_raw()) }
    }

    /// Obtains access to the memory backing `slice` as a Rust slice, assuming
    /// that the task `self` can access it for write. This is used to access task
    /// memory from the kernel in validated form.
    ///
    /// This will treat memory marked `DEVICE` or `DMA` as inaccessible; see
    /// `assert_access` for more details.
    pub fn try_write<'a, T>(
        &'a mut self,
        slice: &'a mut USlice<T>,
    ) -> Result<&'a mut [T], FaultInfo>
    where
        T: FromBytes + Immutable + KnownLayout,
    {
        self.assert_access(
            slice,
            RegionAttributes::WRITE,
            RegionAttributes::DMA,
        )?;

        // Safety: assume_writable requires us to have validated that the
        // slice refers to normal task memory, which we did on the previous
        // line.
        unsafe { Ok(slice.assume_writable()) }
    }

    /// Calls `body` with an iterator over the regions that are currently
    /// hibernated for this task. If the task is not suspended (or has no
    /// hibernated regions), `body` receives an empty iterator and the
    /// hibernation region table is never acquired.
    pub fn with_hibernated_regions<R>(
        &self,
        body: impl FnOnce(BitSliceIter<'_, HibernationRegionDesc>) -> R,
    ) -> R {
        match self.state {
            TaskState::Suspended {
                hibernated_regions, ..
            } => with_hibernation_regions(|regions| {
                body(hibernated_regions.iter(regions))
            }),
            _ => body(BitSliceIter::empty()),
        }
    }

    /// Asserts that this task has access to `slice` as normal memory with
    /// *all* of the given `desired` attributes, and none of the `forbidden`
    /// attributes. This is used to validate kernel accesses to the memory.
    ///
    /// In addition to the `forbidden` attributes passed by the caller, this
    /// will also refuse to access memory marked as `DEVICE`, because such
    /// accesses may be side effecting.
    ///
    /// Most uses of this function also forbid `DMA`, because it is not sound to
    /// create Rust references into `DMA` memory. Access to `DMA` memory is
    /// possible but must use raw pointers and tolerate potential races. (Task
    /// dumps are one of the only cases where this really makes sense.)
    ///
    /// You could call this with `desired` as `RegionAttributes::empty()`; this
    /// would just check that memory is not device, and is a weird thing to do.
    /// A normal call would pass something like `RegionAttributes::READ`.
    ///
    /// Note that all tasks can "access" any empty slice.
    fn assert_access<T>(
        &self,
        slice: &USlice<T>,
        desired: RegionAttributes,
        forbidden: RegionAttributes,
    ) -> Result<(), FaultInfo> {
        // Forceably include DEVICE in the forbidden set, whether or not the
        // caller thought about it.
        let forbidden = forbidden | RegionAttributes::DEVICE;

        // Delegate the actual tests to the kerncore crate, but with our
        // attribute-sensing customization:
        let can_access =
            kerncore::can_access(slice, self.region_table(), |region| {
                region.attributes.contains(desired)
                    && !region.attributes.intersects(forbidden)
            });

        if !can_access {
            return Err(FaultInfo::MemoryAccess {
                address: Some(slice.base_addr() as u32),
                source: FaultSource::Kernel,
            });
        }

        let is_hibernated = self.with_hibernated_regions(|mut regions| {
            regions.any(|r| r.overlaps_slice(slice))
        });

        if is_hibernated {
            return Err(FaultInfo::HibernatedMemoryAccess {
                address: slice.base_addr() as u32,
            });
        }

        Ok(())
    }

    /// Posts a set of notification bits (which might be empty) to this task. If
    /// the task is blocked in receive, and any of the bits match the
    /// notification mask, unblocks the task and returns `true` (indicating that
    /// a context switch may be necessary). If no context switch is required,
    /// returns `false`.
    ///
    /// This would return a `NextTask` but that would require the task to know
    /// its own global ID, which it does not.
    #[must_use]
    pub fn post(&mut self, n: NotificationSet) -> bool {
        self.notifications |= n.0;

        // We only need to check the mask, and make updates, if the task is
        // ready to hear about notifications.
        if self.state.can_accept_notification() {
            if let Some(firing) = self.take_notifications() {
                // A bit the task is interested in has newly become set!
                // Interrupt it.
                self.save.set_recv_result(TaskId::KERNEL, firing, 0, 0, 0);
                self.set_healthy_state(SchedState::Runnable);

                return !self.is_suspended();
            }
        }
        false
    }

    /// Assuming that this task is in or entering a RECV, inspects the RECV
    /// notification mask argument and compares it to the notification bits. If
    /// if any bits are set in both words, clears those bits in the notification
    /// bits and returns them.
    ///
    /// This directly accesses the RECV syscall arguments from the task's saved
    /// state, so it doesn't make sense if the task is not performing a RECV --
    /// but this is not checked.
    pub fn take_notifications(&mut self) -> Option<u32> {
        let args = self.save.as_recv_args();

        let firing = self.notifications & args.notification_mask;
        if firing != 0 {
            self.notifications &= !firing;
            Some(firing)
        } else {
            None
        }
    }

    /// Returns `true` if any of the notification bits in `mask` are set in this
    /// task's notification set.
    ///
    /// This does *not* clear any bits in the task's notification set.
    pub fn has_notifications(&self, mask: u32) -> bool {
        self.notifications & mask != 0
    }

    /// Checks if this task is in a potentially schedulable state.
    pub fn is_runnable(&self) -> bool {
        matches!(self.state, TaskState::Healthy(SchedState::Runnable))
    }

    /// Configures this task's timer.
    ///
    /// `deadline` specifies the moment when the timer should fire, in kernel
    /// time. If `None`, the timer will never fire.
    ///
    /// `notifications` is the set of notification bits to be set when the timer
    /// fires.
    pub fn set_timer(
        &mut self,
        deadline: Option<Timestamp>,
        notifications: NotificationSet,
    ) {
        self.timer.deadline = deadline;
        self.timer.to_post = notifications;
    }

    /// Reads out the state of this task's timer, as previously set by
    /// `set_timer`.
    pub fn timer(&self) -> (Option<Timestamp>, NotificationSet) {
        (self.timer.deadline, self.timer.to_post)
    }

    /// Rewrites this task's state back to its initial form, to effect a task
    /// reboot.
    ///
    /// Note that this only rewrites in-kernel state and relevant parts of
    /// out-of-kernel state (typically, a stack frame stored on the task stack).
    /// This does *not* reinitialize application memory or anything else.
    ///
    /// This does not honor the `START_AT_BOOT` task flag, because this is not a
    /// system reboot. The task will be left in `Stopped` state. If you would
    /// like to run the task after reinitializing it, you must do so explicitly.
    ///
    /// PRECONDITION:
    /// Task must not be suspended.
    pub fn reinitialize(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.timer = TimerState::default();
        self.notifications = 0;
        self.state = TaskState::default();

        crate::arch::reinitialize(self);
    }

    /// Returns a reference to the `TaskDesc` that was used to initially create
    /// this task.
    pub fn descriptor(&self) -> &'static TaskDesc {
        self.descriptor
    }

    /// Returns a reference to the task's memory region descriptor table.
    pub fn region_table(&self) -> &[&'static RegionDesc; REGIONS_PER_TASK] {
        &self.descriptor.regions
    }

    /// Returns this task's current generation number.
    pub fn generation(&self) -> Generation {
        const MASK: u8 = ((1u32 << (16 - TaskId::INDEX_BITS)) - 1) as u8;
        Generation::from(self.generation as u8 & MASK)
    }

    /// Returns this task's priority.
    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// Returns a reference to this task's current state, for inspection.
    pub fn state(&self) -> &TaskState {
        &self.state
    }

    /// Alters this task's state from one healthy state to another.
    ///
    /// To deliver a fault, use `force_fault` instead.
    ///
    /// The only currently supported way of getting a task out of fault state is
    /// `reinitialize`. There are a number of invariants that need to be upheld
    /// when a task begins running, and `reinitialize` gives us a place to
    /// centralize them.
    ///
    /// # Panics
    ///
    /// If you attempt to use this to bring a task out of fault state.
    pub fn set_healthy_state(&mut self, s: SchedState) {
        match &mut self.state {
            // If we are currently in a healthy or hibernated (healthy) state, update it
            TaskState::Healthy(state) => *state = s,
            TaskState::Suspended {
                original_fault_info: None,
                original_healthy_state: state,
                ..
            } => *state = s,

            // otherwise our invariants are broken, panic.
            TaskState::Faulted { .. } => panic!(),
            TaskState::Suspended {
                original_fault_info: Some(_),
                ..
            } => panic!(),
        }
    }

    /// Returns a reference to the saved machine state for the task.
    pub fn save(&self) -> &crate::arch::SavedState {
        &self.save
    }

    /// Returns a mutable reference to the saved machine state for the task.
    pub fn save_mut(&mut self) -> &mut crate::arch::SavedState {
        &mut self.save
    }

    /// Informs a task that a region is now hibernated.
    pub fn hibernate_region(
        &mut self,
        regions: &[HibernationRegionDesc],
        hibernating: usize,
    ) {
        let region = regions[hibernating];

        if !self.region_table().iter().any(|r| region.overlaps(r)) {
            // This region doesn't overlap any of the task's regions, so we
            // don't care about it.
            return;
        }

        match &mut self.state {
            TaskState::Healthy(original_healthy_state) => {
                self.state = TaskState::Suspended {
                    original_fault_info: None,
                    original_healthy_state: *original_healthy_state,
                    hibernated_regions: HibernatedRegionsBitfield::of_region(
                        hibernating as u32,
                    ),
                    debug_request: false,
                };
            }
            TaskState::Faulted {
                fault,
                original_state,
            } => {
                self.state = TaskState::Suspended {
                    original_fault_info: Some(*fault),
                    original_healthy_state: *original_state,
                    hibernated_regions: HibernatedRegionsBitfield::of_region(
                        hibernating as u32,
                    ),
                    debug_request: false,
                };
            }
            TaskState::Suspended {
                hibernated_regions, ..
            } => {
                hibernated_regions.insert(hibernating as u32);
            }
        }
    }

    /// Informs a task that a region is restored
    /// Returns `true` if this caused the task to enter a faulted state.
    pub fn restore_region(
        &mut self,
        restoring: usize,
        mode: RestoreMode,
    ) -> bool {
        let TaskState::Suspended {
            original_fault_info,
            hibernated_regions,
            ..
        } = &mut self.state
        else {
            // We don't care.
            return false;
        };

        // If we are restoring a region that previously blocked us
        // and that region was lost, we should fault ourselves.
        if hibernated_regions.take(restoring as u32)
            && matches!(mode, RestoreMode::Lost)
        {
            match original_fault_info {
                // if we already had a lost region, just add this one to the set
                Some(FaultInfo::LostRegion { regions }) => {
                    regions.insert(restoring as u32)
                }

                // otherwise, overwrite any original fault info with a lost region
                // fault for this region
                other => {
                    *other = Some(FaultInfo::LostRegion {
                        regions: HibernatedRegionsBitfield::of_region(
                            restoring as u32,
                        ),
                    })
                }
            }
        }

        self.try_restore()
    }

    pub fn request_debug(&mut self) {
        match &mut self.state {
            TaskState::Healthy(original_healthy_state) => {
                self.state = TaskState::Suspended {
                    original_fault_info: None,
                    original_healthy_state: *original_healthy_state,
                    hibernated_regions: HibernatedRegionsBitfield::empty(),
                    debug_request: true,
                };
            }
            TaskState::Faulted {
                fault,
                original_state,
            } => {
                self.state = TaskState::Suspended {
                    original_fault_info: Some(*fault),
                    original_healthy_state: *original_state,
                    hibernated_regions: HibernatedRegionsBitfield::empty(),
                    debug_request: true,
                };
            }
            TaskState::Suspended { debug_request, .. } => {
                *debug_request = true;
            }
        }
    }

    /// Returns `true` if this caused the task to enter a faulted state.
    pub fn disable_debug(&mut self) -> bool {
        let TaskState::Suspended { debug_request, .. } = &mut self.state else {
            // We don't care.
            return false;
        };

        *debug_request = false;

        self.try_restore()
    }

    /// Returns `true` if the task transitioned to a faulted state.
    fn try_restore(&mut self) -> bool {
        let TaskState::Suspended {
            original_fault_info,
            original_healthy_state,
            hibernated_regions,
            debug_request,
        } = self.state
        else {
            return false;
        };

        if !hibernated_regions.is_empty() || debug_request {
            return false;
        }

        // Alright! Time to restore.

        // ASSERT: debug_request = false
        //         hibernated_regions = empty

        let buffer = self.suspension_buffer.take();

        self.state = match (original_fault_info, original_healthy_state, buffer)
        {
            // If we have a fault, restore to faulted state with that fault.
            (Some(fault), healthy_state, _) => TaskState::Faulted {
                fault,
                original_state: healthy_state,
            },

            // If we have a pending reply to action, restore to a healthy state
            // and apply the reply
            (None, SchedState::InSuspendedReply(_), Some(buffer)) => {
                match self.save().as_send_args().response {
                    Err(e) => TaskState::Faulted {
                        fault: FaultInfo::SyscallUsage(e),
                        original_state: original_healthy_state,
                    },
                    Ok(mut dest_slice) => {
                        match self.try_write(&mut dest_slice) {
                            Err(fault) => TaskState::Faulted {
                                fault,
                                original_state: original_healthy_state,
                            },
                            Ok(dest) => {
                                let len = buffer.len().min(dest.len());
                                dest[..len].copy_from_slice(&buffer[..len]);
                                TaskState::Healthy(SchedState::Runnable)
                            }
                        }
                    }
                }
            }

            // If we've encountered a strange state, kernel panic
            (None, SchedState::InSuspendedReply(_), None) => {
                panic!("Task in InSuspendedReply state but has no pending reply buffer")
            }
            (None, _, Some(_)) => {
                // while we do expect the Healthy -> Faulted transition
                // while InSuspendedReply, we don't expect to be able to
                // be able to restore from a fault during suspension,
                // or to be able to change our healthy state while
                // InSuspendedReply.
                panic!("Task has pending reply buffer but is in a healthy non-reply state")
            }

            // Otherwise, just restore to the original healthy state.
            (None, healthy_state, None) => TaskState::Healthy(healthy_state),
        };

        matches!(self.state, TaskState::Faulted { .. })
    }

    pub fn is_suspended(&self) -> bool {
        matches!(self.state, TaskState::Suspended { .. })
    }
}

/// Interface that must be implemented by the `arch::SavedState` type. This
/// gives architecture-independent access to task state for the rest of the
/// kernel.
///
/// Architectures need to implement the `argX` and `retX` functions plus
/// `syscall_descriptor`, and the rest of the trait (such as the argument proxy
/// types) will just work.
pub trait ArchState: Default {
    /// TODO: this is probably not needed here.
    fn stack_pointer(&self) -> u32;

    /// Reads syscall argument register 0.
    fn arg0(&self) -> u32;
    /// Reads syscall argument register 1.
    fn arg1(&self) -> u32;
    /// Reads syscall argument register 2.
    fn arg2(&self) -> u32;
    /// Reads syscall argument register 3.
    fn arg3(&self) -> u32;
    /// Reads syscall argument register 4.
    fn arg4(&self) -> u32;
    /// Reads syscall argument register 5.
    fn arg5(&self) -> u32;
    /// Reads syscall argument register 6.
    fn arg6(&self) -> u32;

    /// Reads the syscall descriptor (number).
    fn syscall_descriptor(&self) -> u32;

    /// Writes syscall return argument 0.
    fn ret0(&mut self, _: u32);
    /// Writes syscall return argument 1.
    fn ret1(&mut self, _: u32);
    /// Writes syscall return argument 2.
    fn ret2(&mut self, _: u32);
    /// Writes syscall return argument 3.
    fn ret3(&mut self, _: u32);
    /// Writes syscall return argument 4.
    fn ret4(&mut self, _: u32);
    /// Writes syscall return argument 5.
    fn ret5(&mut self, _: u32);

    /// Interprets arguments as for the SEND syscall and returns the results.
    ///
    /// This is inlined because it's called from several places, and most of
    /// those places only use _part_ of its result -- so inlining it lets most
    /// of its code be eliminated and makes text smaller.
    #[inline(always)]
    fn as_send_args(&self) -> SendArgs {
        SendArgs {
            callee: TaskId((self.arg0() >> 16) as u16),
            operation: self.arg0() as u16,
            message: USlice::from_raw(
                self.arg1() as usize,
                self.arg2() as usize,
            ),
            response: USlice::from_raw(
                self.arg3() as usize,
                self.arg4() as usize,
            ),
            lease_table: USlice::from_raw(
                self.arg5() as usize,
                self.arg6() as usize,
            ),
        }
    }

    /// Interprets arguments as for the RECV syscall and returns the results.
    ///
    /// This is inlined because it's called from several places, and most of
    /// those places only use _part_ of its result -- so inlining it lets most
    /// of its code be eliminated and makes text smaller.
    #[inline(always)]
    fn as_recv_args(&self) -> RecvArgs {
        RecvArgs {
            buffer: USlice::from_raw(
                self.arg0() as usize,
                self.arg1() as usize,
            ),
            notification_mask: self.arg2(),
            specific_sender: {
                let v = self.arg3();
                if v & (1 << 31) != 0 {
                    Some(TaskId(v as u16))
                } else {
                    None
                }
            },
        }
    }

    /// Interprets arguments as for the REPLY syscall and returns the results.
    fn as_reply_args(&self) -> ReplyArgs {
        ReplyArgs {
            callee: TaskId(self.arg0() as u16),
            response_code: self.arg1(),
            message: USlice::from_raw(
                self.arg2() as usize,
                self.arg3() as usize,
            ),
        }
    }

    /// Interprets arguments as for the `REPLY_FAULT` syscall and returns the
    /// results.
    fn as_reply_fault_args(&self) -> ReplyFaultArgs {
        ReplyFaultArgs {
            callee: TaskId(self.arg0() as u16),
            reason: ReplyFaultReason::try_from(self.arg1())
                .map_err(|_| UsageError::BadReplyFaultReason),
        }
    }

    /// Interprets arguments as for the `SET_TIMER` syscall and returns the
    /// results.
    fn as_set_timer_args(&self) -> SetTimerArgs {
        SetTimerArgs {
            deadline: if self.arg0() != 0 {
                Some(Timestamp::from(
                    u64::from(self.arg2()) << 32 | u64::from(self.arg1()),
                ))
            } else {
                None
            },
            notification: NotificationSet(self.arg3()),
        }
    }

    /// Interprets arguments as for the `BORROW_*` family of syscalls and
    /// returns the result.
    fn as_borrow_args(&self) -> BorrowArgs {
        BorrowArgs {
            lender: TaskId(self.arg0() as u16),
            lease_number: self.arg1() as usize,
            offset: self.arg2() as usize,
            buffer: USlice::from_raw(
                self.arg3() as usize,
                self.arg4() as usize,
            ),
        }
    }

    /// Interprets arguments as for the `IRQ_CONTROL` syscall and returns the
    /// results.
    fn as_irq_args(&self) -> IrqArgs {
        IrqArgs {
            notification_bitmask: self.arg0(),
            control: self.arg1(),
        }
    }

    /// Interprets arguments as for the `PANIC` syscall and returns the results.
    fn as_panic_args(&self) -> PanicArgs {
        PanicArgs {
            message: USlice::from_raw(
                self.arg0() as usize,
                self.arg1() as usize,
            ),
        }
    }

    /// Interprets arguments as for the `REFRESH_TASK_ID` syscall and returns
    /// the results.
    fn as_refresh_task_id_args(&self) -> RefreshTaskIdArgs {
        RefreshTaskIdArgs {
            task_id: TaskId(self.arg0() as u16),
        }
    }

    /// Interprets arguments as for the `POST` syscall and returns the results.
    fn as_post_args(&self) -> PostArgs {
        PostArgs {
            task_id: TaskId(self.arg0() as u16),
            notification_bits: NotificationSet(self.arg1()),
        }
    }

    /// Interprets arguments as for the `IRQ_STATUS` syscall and returns the results.
    fn as_irq_status_args(&self) -> IrqStatusArgs {
        IrqStatusArgs {
            notification_bitmask: self.arg0(),
        }
    }

    /// Sets a recoverable error code using the generic ABI.
    fn set_error_response(&mut self, resp: u32) {
        self.ret0(resp);
        self.ret1(0);
    }

    /// Sets the response code and length returned from a SEND.
    fn set_send_response_and_length(&mut self, resp: u32, len: usize) {
        self.ret0(resp);
        self.ret1(len as u32);
    }

    /// Sets the results returned from a RECV.
    fn set_recv_result(
        &mut self,
        sender: TaskId,
        operation: u32,
        length: usize,
        response_capacity: usize,
        lease_count: usize,
    ) {
        self.ret0(0); // currently reserved
        self.ret1(u32::from(sender.0));
        self.ret2(operation);
        self.ret3(length as u32);
        self.ret4(response_capacity as u32);
        self.ret5(lease_count as u32);
    }

    /// Sets the response code and length returned from a BORROW_*.
    fn set_borrow_response_and_length(&mut self, resp: u32, len: usize) {
        self.ret0(resp);
        self.ret1(len as u32);
    }

    /// Sets the response code and info returned from BORROW_INFO.
    fn set_borrow_info(&mut self, atts: u32, len: usize) {
        self.ret0(0);
        self.ret1(atts);
        self.ret2(len as u32);
    }

    /// Sets the results of READ_TIMER.
    fn set_time_result(
        &mut self,
        now: Timestamp,
        dl: Option<Timestamp>,
        not: NotificationSet,
    ) {
        let now_u64 = u64::from(now);
        let dl_u64 = dl.map(u64::from).unwrap_or(0);

        self.ret0(now_u64 as u32);
        self.ret1((now_u64 >> 32) as u32);
        self.ret2(dl.is_some() as u32);
        self.ret3(dl_u64 as u32);
        self.ret4((dl_u64 >> 32) as u32);
        self.ret5(not.0);
    }

    /// Sets the results of REFRESH_TASK_ID
    fn set_refresh_task_id_result(&mut self, id: TaskId) {
        self.ret0(id.0 as u32);
    }

    /// Sets the results of IRQ_STATUS.
    fn set_irq_status_result(&mut self, status: abi::IrqStatus) {
        self.ret0(status.bits());
    }
}

/// Decoded arguments for the `SEND` syscall.
#[derive(Clone, Debug)]
pub struct SendArgs {
    pub callee: TaskId,
    pub operation: u16,
    pub message: Result<USlice<u8>, UsageError>,
    pub response: Result<USlice<u8>, UsageError>,
    pub lease_table: Result<USlice<ULease>, UsageError>,
}

/// Decoded arguments for the `RECV` syscall.
#[derive(Clone, Debug)]
pub struct RecvArgs {
    pub buffer: Result<USlice<u8>, UsageError>,
    pub notification_mask: u32,
    pub specific_sender: Option<TaskId>,
}

/// Decoded arguments for the `REPLY` syscall.
#[derive(Clone, Debug)]
pub struct ReplyArgs {
    pub callee: TaskId,
    pub response_code: u32,
    pub message: Result<USlice<u8>, UsageError>,
}

/// Decoded arguments for the `REPLY_FAULT` syscall.
#[derive(Clone, Debug)]
pub struct ReplyFaultArgs {
    pub callee: TaskId,
    pub reason: Result<ReplyFaultReason, UsageError>,
}

/// Decoded arguments for the `SET_TIMER` syscall.
#[derive(Clone, Debug)]
pub struct SetTimerArgs {
    pub deadline: Option<Timestamp>,
    pub notification: NotificationSet,
}

/// Decoded arguments for the `BORROW_*` syscalls.
#[derive(Clone, Debug)]
pub struct BorrowArgs {
    pub lender: TaskId,
    pub lease_number: usize,
    pub offset: usize,
    pub buffer: Result<USlice<u8>, UsageError>,
}

/// Decoded arguments for the `IRQ_CONTROL` syscall.
#[derive(Clone, Debug)]
pub struct IrqArgs {
    pub notification_bitmask: u32,
    pub control: u32,
}

/// Decoded arguments for the `PANIC` syscall.
#[derive(Clone, Debug)]
pub struct PanicArgs {
    pub message: Result<USlice<u8>, UsageError>,
}

/// Decoded arguments for the `REFRESH_TASK_ID` syscall.
#[derive(Clone, Debug)]
pub struct RefreshTaskIdArgs {
    pub task_id: TaskId,
}

/// Decoded arguments for the `POST` syscall.
#[derive(Clone, Debug)]
pub struct PostArgs {
    pub task_id: TaskId,
    pub notification_bits: NotificationSet,
}

/// Decoded arguments for the `IRQ_STATUS` syscall.
#[derive(Clone, Debug)]
pub struct IrqStatusArgs {
    pub notification_bitmask: u32,
}

/// State for a task timer.
///
/// Task timers are used to multiplex the hardware timer.
#[derive(Debug, Default)]
pub struct TimerState {
    /// Deadline, in kernel time, at which this timer should fire. If `None`,
    /// the timer is disabled.
    deadline: Option<Timestamp>,
    /// Set of notification bits to post to the owning task when this timer
    /// fires.
    to_post: NotificationSet,
}

/// Collection of bits that may be posted to a task's notification word.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
#[repr(transparent)]
pub struct NotificationSet(pub u32);

/// Return value for operations that can have scheduling implications. This is
/// marked `must_use` because forgetting to actually update the scheduler after
/// performing an operation that requires it would be Bad.
#[derive(Clone, Debug, Eq, PartialEq)]
#[must_use]
pub enum NextTask {
    /// It's fine to keep running whatever task we were just running.
    Same,
    /// We need to switch tasks, but this routine has not concluded which one
    /// should now run. The scheduler needs to figure it out.
    Other,
    /// We need to switch tasks, and we already know which one should run next.
    /// This is an optimization available in certain IPC cases.
    Specific(usize),
}

impl NextTask {
    pub fn combine(self, other: Self) -> Self {
        use NextTask::*; // shorthand for patterns

        match (self, other) {
            // If both agree, our job is easy.
            (x, y) if x == y => x,
            // Specific task recommendations that *don't* agree get downgraded
            // to Other.
            (Specific(_), Specific(_)) => Other,
            // If only *one* is specific, it wins.
            (Specific(x), _) | (_, Specific(x)) => Specific(x),
            // Otherwise, if either suggestion says switch, switch.
            (Other, _) | (_, Other) => Other,
            // All we have left is...
            (Same, Same) => Same,
        }
    }
}

/// Processes all enabled timers in the task table, posting notifications for
/// any that have expired by `current_time` (and disabling them atomically).
pub fn process_timers(tasks: &mut [Task], current_time: Timestamp) -> NextTask {
    let mut sched_hint = NextTask::Same;
    for (index, task) in tasks.iter_mut().enumerate() {
        if let Some(deadline) = task.timer.deadline {
            if deadline <= current_time {
                task.timer.deadline = None;
                let task_hint = if task.post(task.timer.to_post) {
                    NextTask::Specific(index)
                } else {
                    NextTask::Same
                };
                sched_hint = sched_hint.combine(task_hint)
            }
        }
    }
    sched_hint
}

/// Checks a user-provided `TaskId` for validity against `table`.
///
/// On success, returns an index that can be used to dereference `table` without
/// panicking.
///
/// On failure, indicates the condition by `UserError`.
pub fn check_task_id_against_table(
    table: &[Task],
    id: TaskId,
) -> Result<usize, UserError> {
    if id.index() >= table.len() {
        return Err(FaultInfo::SyscallUsage(UsageError::TaskOutOfRange).into());
    }

    // Check for dead task ID.
    let table_generation = table[id.index()].generation();

    if table_generation != id.generation() {
        let code = abi::dead_response_code(table_generation);

        return Err(UserError::Recoverable(code, NextTask::Same));
    }

    Ok(id.index())
}

/// Selects a new task to run after `previous`. Tries to be fair, kind of.
///
/// If no tasks are runnable, the kernel panics.
pub fn select(previous: usize, tasks: &[Task]) -> &Task {
    match priority_scan(previous, tasks, |t| t.is_runnable()) {
        Some((_index, task)) => task,
        None => panic!(),
    }
}

/// Scans the task table to find a prioritized candidate.
///
/// Scans `tasks` for the next task, after `previous`, that satisfies `pred`. If
/// more than one task satisfies `pred`, returns the most important one. If
/// multiple tasks with the same priority satisfy `pred`, prefers the first one
/// in order after `previous`, mod `tasks.len()`. Finally, if no tasks satisfy
/// `pred`, returns `None`
///
/// Whew.
///
/// This is generally the right way to search a task table, and is used to
/// implement (among other bits) the scheduler.
///
/// On success, the return value is the task's index in the task table, and a
/// direct reference to the task.
pub fn priority_scan(
    previous: usize,
    tasks: &[Task],
    pred: impl Fn(&Task) -> bool,
) -> Option<(usize, &Task)> {
    let mut pos = previous;
    let mut choice: Option<(usize, &Task)> = None;
    for _step_no in 0..tasks.len() {
        pos = pos.wrapping_add(1);
        if pos >= tasks.len() {
            pos = 0;
        }
        let t = &tasks[pos];
        if !pred(t) {
            continue;
        }

        if let Some((_, best_task)) = choice {
            if !t.priority.is_more_important_than(best_task.priority) {
                continue;
            }
        }

        choice = Some((pos, t));
    }

    choice
}

/// Puts a task into a forced fault condition.
///
/// The task is designated by the `index` parameter. We need access to the
/// entire task table, as well as the designated task, so that we can take the
/// opportunity to notify the supervisor.
///
/// The task will not be scheduled again until the fault is cleared. The
/// kernel won't clear faults on its own, it must be asked.
///
/// If the task is already faulted, we will retain the information about
/// what state the task was in *before* it faulted, and *erase* the last
/// fault. These kinds of double-faults are expected to be super rare.
///
/// Returns a `NextTask` under the assumption that, if you're hitting tasks
/// with faults, at least one of them is probably the current task; this
/// makes it harder to forget to request rescheduling. If you're faulting
/// some other task you can explicitly ignore the result.
pub fn force_fault(
    tasks: &mut [Task],
    index: usize,
    fault: FaultInfo,
) -> NextTask {
    let task = &mut tasks[index];

    task.state = match task.state {
        TaskState::Healthy(sched) => TaskState::Faulted {
            original_state: sched,
            fault,
        },
        TaskState::Faulted { original_state, .. } => {
            // Double fault - fault while faulted
            // Original fault information is lost
            TaskState::Faulted {
                fault,
                original_state,
            }
        }
        TaskState::Suspended {
            original_healthy_state,
            debug_request,
            hibernated_regions,
            original_fault_info: _,
        } => TaskState::Suspended {
            original_healthy_state,
            original_fault_info: Some(fault),
            hibernated_regions,
            debug_request,
        },
    };

    let supervisor_awoken = !task.is_suspended()
        && tasks[0].post(NotificationSet(HUBRIS_FAULT_NOTIFICATION));

    if supervisor_awoken {
        NextTask::Specific(0)
    } else {
        NextTask::Other
    }
}

/// Produces a current `TaskId` (i.e. one with the correct generation) for
/// `tasks[index]`.
pub fn current_id(tasks: &[Task], index: usize) -> TaskId {
    TaskId::for_index_and_gen(index, tasks[index].generation())
}

/// Reads reply data from the caller's memory and buffers it on the
/// suspended callee, to be delivered when the callee's region is restored.
///
/// Returns the number of bytes buffered, matching `safe_copy`'s contract.
pub fn set_suspension_reply(
    tasks: &mut [Task],
    caller: usize,
    src_slice: USlice<u8>,
    callee: usize,
    _dest_slice: USlice<u8>,
) -> Result<usize, crate::err::InteractFault> {
    let data = tasks[caller]
        .try_read(&src_slice)
        .map_err(crate::err::InteractFault::in_src)?;

    let buffer = SuspensionBuffer::ok(data)
        .expect("reply exceeded SuspensionBuffer capacity after size check");

    let len = buffer.len();
    tasks[callee].suspension_buffer = Some(buffer);
    Ok(len)
}
