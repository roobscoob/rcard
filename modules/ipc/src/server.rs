use core::mem::MaybeUninit;
use userlib::{Message, ReplyFaultReason};

use crate::split_opcode;

#[cfg(feature = "dangerously_enable_uart3_debugging")]
pub mod debug_uart {
    use core::fmt::Write;

    const USART3_BASE: usize = 0x5008_6000;
    const CR1: usize = USART3_BASE + 0x00;
    const BRR: usize = USART3_BASE + 0x0C;
    const ISR: usize = USART3_BASE + 0x1C;
    const TDR: usize = USART3_BASE + 0x28;

    pub struct Uart3;

    impl Uart3 {
        pub fn init() {
            unsafe {
                core::ptr::write_volatile(BRR as *mut u32, 0x1A1); // 48MHz / 115200
                core::ptr::write_volatile(CR1 as *mut u32, (1 << 0) | (1 << 3)); // UE | TE
            }
        }
    }

    impl Write for Uart3 {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for b in s.bytes() {
                unsafe {
                    while core::ptr::read_volatile(ISR as *const u32) & (1 << 7) == 0 {}
                    core::ptr::write_volatile(TDR as *mut u32, b as u32);
                }
            }
            Ok(())
        }
    }
}

/// Trait implemented by generated dispatcher structs for each resource.
pub trait ResourceDispatch {
    /// Dispatch a method call given the method ID and incoming message.
    fn dispatch(&mut self, method_id: u8, msg: &Message<'_>) -> Result<(), ReplyFaultReason>;

    /// Remove all resources owned by the given task index.
    /// Called when the server detects that a client has restarted.
    fn cleanup_client(&mut self, task_index: u16);
}

/// Per-client generation tracker entry.
struct ClientGen {
    task_index: u16,
    generation: userlib::Gen,
    active: bool,
}

/// IPC server that routes incoming messages to resource dispatchers.
///
/// The client tracking array is sized to `TASK_COUNT`, which is set by
/// the Hubris build system via the `HUBRIS_TASKS` environment variable.
pub struct Server<'a, const MAX_RESOURCES: usize> {
    dispatchers: [Option<(u8, &'a mut dyn ResourceDispatch)>; MAX_RESOURCES],
    count: usize,
    clients: [ClientGen; crate::TASK_COUNT],
    acl_fn: fn(u16) -> bool,
}

impl<'a, const MAX_RESOURCES: usize> Server<'a, MAX_RESOURCES> {
    pub fn new(acl_fn: fn(u16) -> bool) -> Self {
        Self {
            dispatchers: [const { None }; MAX_RESOURCES],
            count: 0,
            clients: [const {
                ClientGen {
                    task_index: 0,
                    generation: userlib::Gen::DEFAULT,
                    active: false,
                }
            }; crate::TASK_COUNT],
            acl_fn,
        }
    }

    /// Register a resource dispatcher for a given kind byte.
    pub fn register(&mut self, kind: u8, dispatcher: &'a mut dyn ResourceDispatch) {
        assert!(
            !self.dispatchers[..self.count]
                .iter()
                .any(|e| matches!(e, Some((k, _)) if *k == kind)),
            "ipc::Server: duplicate dispatcher registered for kind 0x{:02X}",
            kind,
        );
        assert!(
            self.count < MAX_RESOURCES,
            "ipc::Server: MAX_RESOURCES ({}) exceeded",
            MAX_RESOURCES
        );
        self.dispatchers[self.count] = Some((kind, dispatcher));
        self.count += 1;
    }

    pub fn with_dispatcher(mut self, kind: u8, dispatcher: &'a mut dyn ResourceDispatch) -> Self {
        self.register(kind, dispatcher);
        self
    }

    /// Check if a client's generation has changed (indicating restart).
    /// If so, clean up all resources for that client across all dispatchers.
    fn check_client_generation(&mut self, sender: userlib::TaskId) {
        let task_index = sender.task_index();
        let sender_gen = sender.generation();

        // Find existing entry for this task index.
        for c in self.clients.iter_mut() {
            if c.active && c.task_index == task_index {
                if c.generation != sender_gen {
                    // Client restarted — clean up its resources.
                    for entry in self.dispatchers.iter_mut() {
                        if let Some((_, d)) = entry.as_mut() {
                            d.cleanup_client(task_index);
                        }
                    }
                    c.generation = sender_gen;
                }
                return;
            }
        }

        // New client — find an empty slot.
        for c in self.clients.iter_mut() {
            if !c.active {
                c.task_index = task_index;
                c.generation = sender_gen;
                c.active = true;
                return;
            }
        }
        panic!(
            "ipc::Server: TASK_COUNT ({}) exceeded — task index {} has no tracking slot",
            crate::TASK_COUNT,
            task_index,
        );
    }

    /// Run the server loop forever, dispatching incoming messages.
    /// Does not handle notification bits. See [`run_with_notifications`] for that.
    pub fn run(&mut self, buf: &mut [MaybeUninit<u8>]) -> ! {
        self.run_with_notifications(buf, 0, |_| {})
    }

    /// Run the server loop forever, dispatching incoming messages and
    /// notification bits.
    ///
    /// When `notification_mask` is non-zero, the server uses `sys_recv_open`
    /// to also listen for kernel notification bits. When a notification fires,
    /// `on_notification` is called with the received bits.
    pub fn run_with_notifications(
        &mut self,
        buf: &mut [MaybeUninit<u8>],
        notification_mask: u32,
        mut on_notification: impl FnMut(u32),
    ) -> ! {
        #[cfg(feature = "dangerously_enable_uart3_debugging")]
        debug_uart::Uart3::init();

        loop {
            if notification_mask == 0 {
                let msg = userlib::sys_recv_msg_open(buf);
                self.dispatch_message(&msg);
            } else {
                match userlib::sys_recv_open(buf, notification_mask) {
                    userlib::MessageOrNotification::Notification(bits) => {
                        on_notification(bits);
                    }
                    userlib::MessageOrNotification::Message(msg) => {
                        self.dispatch_message(&msg);
                    }
                }
            }
        }
    }

    fn dispatch_message(&mut self, msg: &userlib::Message<'_>) {
        self.check_client_generation(msg.sender);

        if !(self.acl_fn)(msg.sender.task_index()) {
            userlib::sys_reply(msg.sender, crate::ACCESS_VIOLATION, &[]);
            return;
        }

        let (kind, method) = split_opcode(msg.operation);

        #[cfg(feature = "dangerously_enable_uart3_debugging")]
        {
            use core::fmt::Write;
            let _ = write!(
                debug_uart::Uart3,
                "[ipc k=0x{kind:02x} m=0x{method:02x} from={:?} leases={}]\n",
                msg.sender, msg.lease_count,
            );
        }

        // notify_dead: client is about to panic — clean up all its resources
        // across every dispatcher. Kind byte is ignored.
        if method == crate::handle::NOTIFY_DEAD_METHOD {
            let task_index = msg.sender.task_index();
            for entry in self.dispatchers.iter_mut() {
                if let Some((_, d)) = entry.as_mut() {
                    d.cleanup_client(task_index);
                }
            }
            userlib::sys_reply(msg.sender, userlib::ResponseCode::SUCCESS, &[]);
            return;
        }

        let result = self.dispatchers.iter_mut().find_map(|entry| {
            let (k, d) = entry.as_mut()?;
            if *k == kind {
                Some(d.dispatch(method, msg))
            } else {
                None
            }
        });

        match result {
            Some(Ok(())) => {
                #[cfg(feature = "dangerously_enable_uart3_debugging")]
                { use core::fmt::Write; let _ = write!(debug_uart::Uart3, "[ipc ok]\n"); }
            }
            Some(Err(e)) => {
                #[cfg(feature = "dangerously_enable_uart3_debugging")]
                { use core::fmt::Write; let _ = write!(debug_uart::Uart3, "[ipc DISPATCH ERR k=0x{kind:02x} m=0x{method:02x}]\n"); }
                // The client sent a malformed message (bad size, bad
                // contents, or bad leases). Reply with non-SUCCESS so the
                // client's generated panic handler fires — its panic handler
                // will log and/or restart the task. The server continues.
                let _ = e;
                userlib::sys_reply(msg.sender, crate::MALFORMED_MESSAGE, &[]);
            }
            None => {
                #[cfg(feature = "dangerously_enable_uart3_debugging")]
                { use core::fmt::Write; let _ = write!(debug_uart::Uart3, "[ipc NO DISPATCHER k=0x{kind:02x}]\n"); }
                // Client sent a kind byte we have no dispatcher for —
                // malformed message. Same treatment as bad contents:
                // reply non-SUCCESS to fire the client's panic handler.
                userlib::sys_reply(msg.sender, crate::MALFORMED_MESSAGE, &[]);
            }
        }
    }
}
