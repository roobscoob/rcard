#![no_std]
#![no_main]

use core::cell::UnsafeCell;
use sysmodule_reactor_api::*;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
}

const QUEUE_CAPACITY: usize = 32;

struct State {
    queue: heapless::Vec<Notification, QUEUE_CAPACITY>,
    cursors: [u8; ipc::TASK_COUNT],
}

/// Wrapper to allow a non-`Sync` type in a `static`.
/// Safety: this sysmodule is single-threaded and the server dispatch is
/// non-reentrant, so no concurrent access can occur.
struct SyncCell(UnsafeCell<State>);
unsafe impl Sync for SyncCell {}

static STATE: SyncCell = SyncCell(UnsafeCell::new(State {
    queue: heapless::Vec::new(),
    cursors: [0; ipc::TASK_COUNT],
}));

/// Obtain an exclusive reference to the reactor state.
///
/// Safety: only valid in a single-threaded, non-reentrant context —
/// which is exactly how the IPC server dispatch works.
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    unsafe { f(&mut *STATE.0.get()) }
}

/// Well-known operation code matching the supervisor's OP_DROP_REPORT.
const OP_DROP_REPORT: u16 = 0xDEAD;

/// Report that a notification was dropped for a specific subscriber.
///
/// `dropped_for` is the task index of the subscriber that will not receive
/// this notification (evicted before they consumed it).
///
/// Wire format (11 bytes LE):
///   [0..2]  sender_index: u16
///   [2..4]  group_id: u16
///   [4..8]  code: u32
///   [8]     priority: u8
///   [9..11] dropped_for: u16
fn drop_notification_for(notif: &Notification, dropped_for: u16) {
    let mut msg = [0u8; 11];
    msg[0..2].copy_from_slice(&notif.sender_index.to_le_bytes());
    msg[2..4].copy_from_slice(&notif.group_id.to_le_bytes());
    msg[4..8].copy_from_slice(&notif.code.to_le_bytes());
    msg[8] = notif.priority;
    msg[9..11].copy_from_slice(&dropped_for.to_le_bytes());

    let sup = userlib::TaskId::gen0(0);
    let _ = userlib::sys_send(sup, OP_DROP_REPORT, &msg, &mut [], &mut []);
}

fn validate(sender_index: u16, group_id: u16, priority: u8) -> Result<(), ReactorError> {
    let group = generated::GROUPS
        .get(group_id as usize)
        .ok_or(ReactorError::InvalidGroup)?;
    if !generated::is_sender_allowed(group_id, sender_index) {
        return Err(ReactorError::NotAuthorized);
    }
    if !group.priority_range.contains(&priority) {
        return Err(ReactorError::PriorityOutOfRange);
    }
    Ok(())
}

/// Report a drop for every subscriber of `notif`'s group whose cursor
/// hasn't yet passed `index` (i.e. they would have seen this entry).
fn report_eviction(state: &State, notif: &Notification, index: usize) {
    for &task_idx in generated::group_subscribers(notif.group_id) {
        let cursor = state.cursors[task_idx as usize] as usize;
        if cursor <= index {
            drop_notification_for(notif, task_idx);
        }
    }
}

/// Try to evict one entry to make room for a notification with the given priority.
fn evict(state: &mut State, priority: u8, strategy: OverflowStrategy) -> bool {
    match strategy {
        OverflowStrategy::Reject => false,
        OverflowStrategy::DropOldest => {
            if let Some(i) = state.queue.iter().position(|n| n.priority <= priority) {
                report_eviction(state, &state.queue[i].clone(), i);
                remove_and_adjust_cursors(state, i);
                true
            } else {
                false
            }
        }
        OverflowStrategy::DropNewest => {
            if let Some(i) = state.queue.iter().rposition(|n| n.priority <= priority) {
                report_eviction(state, &state.queue[i].clone(), i);
                remove_and_adjust_cursors(state, i);
                true
            } else {
                false
            }
        }
    }
}

/// Remove entry at `index` from the queue and adjust all cursors accordingly.
fn remove_and_adjust_cursors(state: &mut State, index: usize) {
    state.queue.remove(index);
    for &task_idx in generated::SUBSCRIBER_TASKS {
        let c = &mut state.cursors[task_idx as usize];
        if (*c as usize) > index {
            *c -= 1;
        }
    }
}

/// Garbage-collect entries from the front of the queue.
///
/// An entry is collectable when every subscriber *of that entry's group*
/// has a cursor past it.  Subscribers to other groups are irrelevant —
/// they will never consume that entry, so their cursor position must not
/// block collection.
fn gc(state: &mut State) {
    while !state.queue.is_empty() {
        let group_id = state.queue[0].group_id;
        let all_consumed = generated::group_subscribers(group_id)
            .iter()
            .all(|&t| state.cursors[t as usize] > 0);
        if !all_consumed {
            break;
        }
        state.queue.remove(0);
        // Decrement all subscriber cursors (not just group subscribers)
        // since queue indices shifted.
        for &task_idx in generated::SUBSCRIBER_TASKS {
            let c = &mut state.cursors[task_idx as usize];
            if *c > 0 {
                *c -= 1;
            }
        }
    }
}

/// Post NOTIFICATION_BIT to all subscribers of a group via sys_post.
///
/// TODO: This uses generation 0 to construct the TaskId. If a subscriber task
/// has been restarted (generation incremented), sys_post will return TaskDeath
/// and the notification will be silently lost. There is no kernel API to look up
/// the current generation for a task index. To fix this properly, the subscriber
/// registration should store the full TaskId (including generation) so that
/// notify can use the correct generation, or the subscriber should re-register
/// after restart.
fn notify_subscribers(group_id: u16) {
    for &task_idx in generated::group_subscribers(group_id) {
        let tid = userlib::TaskId::gen0(task_idx);
        let _ = userlib::sys_post(tid, NOTIFICATION_BIT);
    }
}

/// Check if the calling task has more notifications past its cursor.
fn has_more_for(state: &State, task_index: u16) -> bool {
    let cursor = state.cursors[task_index as usize] as usize;
    state.queue[cursor..]
        .iter()
        .any(|n| generated::is_subscriber(n.group_id, task_index))
}

struct ReactorImpl;

impl Reactor for ReactorImpl {
    fn push(
        meta: ipc::Meta,
        group_id: u16,
        code: u32,
        priority: u8,
        strategy: OverflowStrategy,
    ) -> Result<(), ReactorError> {
        let sender_index = meta.sender.task_index();
        validate(sender_index, group_id, priority)?;

        let notif = Notification {
            sender_index,
            group_id,
            code,
            priority,
        };

        with_state(|state| {
            if state.queue.is_full() {
                gc(state);
            }

            if state.queue.is_full() && !evict(state, priority, strategy) {
                for &t in generated::group_subscribers(group_id) {
                    drop_notification_for(&notif, t);
                }
                return Err(ReactorError::QueueFull);
            }

            #[allow(clippy::unwrap_used)]
            state.queue.push(notif).unwrap();
            notify_subscribers(group_id);
            Ok(())
        })
    }

    fn refresh(
        meta: ipc::Meta,
        group_id: u16,
        code: u32,
        priority: u8,
        strategy: OverflowStrategy,
    ) -> Result<(), ReactorError> {
        let sender_index = meta.sender.task_index();
        validate(sender_index, group_id, priority)?;

        with_state(|state| {
            // Remove existing match if found, keeping the max priority.
            let mut effective_priority = priority;
            if let Some(i) = state.queue.iter().position(|n| {
                n.sender_index == sender_index && n.group_id == group_id && n.code == code
            }) {
                effective_priority = effective_priority.max(state.queue[i].priority);
                remove_and_adjust_cursors(state, i);
            }

            let notif = Notification {
                sender_index,
                group_id,
                code,
                priority: effective_priority,
            };

            if state.queue.is_full() {
                gc(state);
            }
            if state.queue.is_full() && !evict(state, effective_priority, strategy) {
                for &t in generated::group_subscribers(group_id) {
                    drop_notification_for(&notif, t);
                }
                return Err(ReactorError::QueueFull);
            }

            #[allow(clippy::unwrap_used)]
            state.queue.push(notif).unwrap();
            notify_subscribers(group_id);
            Ok(())
        })
    }

    fn pull(meta: ipc::Meta) -> Option<Notification> {
        let task_index = meta.sender.task_index();

        with_state(|state| {
            // GC before scanning so we don't walk past consumed entries.
            gc(state);

            let start = state.cursors[task_index as usize] as usize;

            // Scan from cursor for the first notification this task subscribes to.
            for i in start..state.queue.len() {
                if generated::is_subscriber(state.queue[i].group_id, task_index) {
                    let notif = state.queue[i];

                    // Advance cursor.
                    state.cursors[task_index as usize] = (i + 1) as u8;

                    gc(state);

                    // Re-post if more remain for this subscriber.
                    if has_more_for(state, task_index) {
                        let _ = userlib::sys_post(meta.sender, NOTIFICATION_BIT);
                    }

                    return Some(notif);
                }
            }

            None
        })
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    userlib::sys_panic(b"reactor panic")
}

#[export_name = "main"]
fn main() -> ! {
    ipc::server! {
        Reactor: ReactorImpl,
    }
}
