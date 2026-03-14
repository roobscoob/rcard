#![no_std]

/// The notification bit the reactor posts to subscriber tasks.
/// Uses the MSB (bit 31) to avoid colliding with application-defined bits.
pub const NOTIFICATION_BIT: u32 = 1 << 31;

/// A notification queued for delivery to a notification group.
///
/// The reactor preserves the original sender's identity so the target
/// can distinguish who sent the notification.
#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::FromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(C, packed)]
pub struct Notification {
    /// Task index of the original sender (preserved from IPC metadata).
    pub sender_index: u16,
    /// Notification group to deliver to.
    pub group_id: u16,
    /// An application-defined notification code.
    pub code: u32,
    /// Priority of this notification. Higher values = higher priority.
    pub priority: u8,
}

/// What to do when the queue is full.
#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(u8)]
pub enum OverflowStrategy {
    /// Reject the new notification if there's no space.
    Reject = 0,
    /// Drop the oldest entry with priority <= the new notification's priority.
    DropOldest = 1,
    /// Drop the newest entry with priority <= the new notification's priority.
    DropNewest = 2,
}

#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(u8)]
pub enum ReactorError {
    /// The sender is not allowed to push to this notification group.
    NotAuthorized = 0,
    /// The priority is outside the group's allowed range.
    PriorityOutOfRange = 1,
    /// The notification group does not exist.
    InvalidGroup = 2,
    /// The queue is full and no entry could be evicted.
    QueueFull = 3,
}

#[ipc::resource(arena_size = 0, kind = 0x05)]
pub trait Reactor {
    /// Queue a notification for delivery to a group. Returns immediately.
    #[message]
    fn push(
        group_id: u16,
        code: u32,
        priority: u8,
        strategy: OverflowStrategy,
    ) -> Result<(), ReactorError>;

    /// If a matching (sender, group, code) notification exists, remove it and
    /// re-push with the new priority. Otherwise, push normally (using the
    /// overflow strategy if the queue is full).
    #[message]
    fn refresh(
        group_id: u16,
        code: u32,
        priority: u8,
        strategy: OverflowStrategy,
    ) -> Result<(), ReactorError>;

    /// Pull the next queued notification for the calling task.
    /// The reactor tracks a per-client cursor; only notifications for groups
    /// the caller subscribes to are returned.
    /// If more notifications remain, the reactor re-posts NOTIFICATION_BIT.
    #[message]
    fn pull() -> Option<Notification>;
}
