use core::mem::MaybeUninit;

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
                core::ptr::write_volatile(BRR as *mut u32, 0x823); // 240MHz / 115200
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
    ///
    /// The `reply` token MUST be consumed (via `reply_ok`, `reply_error`,
    /// or `reply_serialize`). If dropped without consuming, a fault is sent.
    fn dispatch(
        &mut self,
        method_id: u8,
        msg: crate::dispatch::MessageData<'_>,
        reply: crate::dispatch::PendingReply,
    );

    /// Remove all resources owned by the given task index.
    /// Called when the server detects that a client has restarted.
    fn cleanup_client(&mut self, task_index: u16);
}

/// Per-client generation tracker entry.
struct ClientGen {
    task_index: u16,
    generation: crate::kern::Gen,
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
                    generation: crate::kern::Gen::DEFAULT,
                    active: false,
                }
            }; crate::TASK_COUNT],
            acl_fn,
        }
    }

    /// Register a resource dispatcher for a given kind byte.
    pub fn register(&mut self, kind: u8, dispatcher: &'a mut dyn ResourceDispatch) {
        // SAFETY: self.count <= MAX_RESOURCES is maintained by this function.
        let registered = unsafe { self.dispatchers.get_unchecked(..self.count) };
        if registered
            .iter()
            .any(|e| matches!(e, Some((k, _)) if *k == kind))
        {
            crate::__ipc_panic!("ipc::Server: duplicate dispatcher registered for kind {}", kind);
        }
        if self.count >= MAX_RESOURCES {
            crate::__ipc_panic!("ipc::Server: MAX_RESOURCES exceeded");
        }
        // SAFETY: self.count < MAX_RESOURCES proven above.
        unsafe { *self.dispatchers.get_unchecked_mut(self.count) = Some((kind, dispatcher)) };
        self.count += 1;
    }

    pub fn with_dispatcher(mut self, kind: u8, dispatcher: &'a mut dyn ResourceDispatch) -> Self {
        self.register(kind, dispatcher);
        self
    }

    /// Check if a client's generation has changed (indicating restart).
    /// If so, clean up all resources for that client across all dispatchers.
    fn check_client_generation(&mut self, sender: crate::kern::TaskId) {
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

        #[allow(clippy::panic)]
        {
            panic!("ipc::Server: TASK_COUNT exceeded");
        }
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
                let msg = crate::kern::sys_recv_msg_open(buf);
                self.dispatch_message(&msg);
            } else {
                match crate::kern::sys_recv_open(buf, notification_mask) {
                    crate::kern::MessageOrNotification::Notification(bits) => {
                        on_notification(bits);
                    }
                    crate::kern::MessageOrNotification::Message(msg) => {
                        self.dispatch_message(&msg);
                    }
                }
            }
        }
    }

    fn dispatch_message(&mut self, msg: &crate::kern::Message<'_>) {
        self.check_client_generation(msg.sender);

        if !(self.acl_fn)(msg.sender.task_index()) {
            crate::kern::sys_reply(msg.sender, crate::ACCESS_VIOLATION, &[]);
            return;
        }

        let (kind, method) = split_opcode(msg.operation);

        #[cfg(feature = "dangerously_enable_uart3_debugging")]
        {
            use core::fmt::Write;
            let _ = write!(
                debug_uart::Uart3,
                "[ipc k=0x{kind:02x} m=0x{method:02x} from={:?} leases={}]\n",
                msg.sender,
                msg.lease_count,
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
            crate::kern::sys_reply(msg.sender, crate::kern::ResponseCode::SUCCESS, &[]);
            return;
        }

        // Split the raw message into a typed MessageData + PendingReply.
        let (msg_data, reply) = match crate::dispatch::split_message(msg) {
            Ok(pair) => pair,
            Err(reason) => {
                crate::kern::sys_reply_fault(msg.sender, reason);
                return;
            }
        };

        // Find the dispatcher for this kind byte.
        let found = self.dispatchers.iter_mut().find_map(|entry| {
            let (k, d) = entry.as_mut()?;
            if *k == kind { Some(d) } else { None }
        });

        match found {
            Some(d) => {
                d.dispatch(method, msg_data, reply);
                #[cfg(feature = "dangerously_enable_uart3_debugging")]
                {
                    use core::fmt::Write;
                    let _ = write!(debug_uart::Uart3, "[ipc ok]\n");
                }
            }
            None => {
                #[cfg(feature = "dangerously_enable_uart3_debugging")]
                {
                    use core::fmt::Write;
                    let _ = write!(debug_uart::Uart3, "[ipc NO DISPATCHER k=0x{kind:02x}]\n");
                }
                reply.reply_error(crate::MALFORMED_MESSAGE, &[]);
            }
        }
    }
}
