#![no_std]
#![allow(unused_variables)]

/// Task slot indices and names.
pub mod tasks {
    include!(concat!(env!("OUT_DIR"), "/tasks.rs"));
}

/// Notification group IDs, ACLs, subscriber lists.
pub mod notifications {
    include!(concat!(env!("OUT_DIR"), "/notifications.rs"));
}

/// Task slot bindings — replacement for hubris-task-slots.
pub mod slots {
    include!(concat!(env!("OUT_DIR"), "/slots.rs"));
}

/// Peer references — tasks referenced via `peers` in task.ncl.
/// Fields are `Option<TaskId>`: `Some` if the peer is in the build, `None` if not.
pub mod peers {
    include!(concat!(env!("OUT_DIR"), "/peers.rs"));
}

/// IPC access control lists.
pub mod acl {
    include!(concat!(env!("OUT_DIR"), "/acl.rs"));
}

/// Per-task IRQ → notification bit mapping.
/// Use via the `generated::irq_bit!(crate_name, irq_name)` macro.
pub mod irqs {
    include!(concat!(env!("OUT_DIR"), "/irqs.rs"));
}

/// Build identity — build UUID and version string.
pub mod build_info {
    include!(concat!(env!("OUT_DIR"), "/build_info.rs"));
}
