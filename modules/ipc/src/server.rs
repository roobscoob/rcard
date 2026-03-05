use core::mem::MaybeUninit;
use userlib::{Message, ReplyFaultReason};

use crate::split_opcode;

/// Trait implemented by generated dispatcher structs for each resource.
pub trait ResourceDispatch {
    /// Dispatch a method call given the method ID and incoming message.
    fn dispatch(&mut self, method_id: u8, msg: &Message<'_>) -> Result<(), ReplyFaultReason>;

    /// Remove all resources owned by the given task index.
    /// Called when the server detects that a client has restarted.
    fn cleanup_client(&mut self, task_index: u16);
}

/// Maximum number of resource kinds a server can handle.
const BASE_MAX_RESOURCES: usize = 16;

/// Maximum number of client task generations tracked.
const BASE_MAX_CLIENTS: usize = 16;

/// Per-client generation tracker entry.
struct ClientGen {
    task_index: u16,
    generation: userlib::Gen,
    active: bool,
}

/// IPC server that routes incoming messages to resource dispatchers.
pub struct Server<
    'a,
    const MAX_RESOURCES: usize = BASE_MAX_RESOURCES,
    const MAX_CLIENTS: usize = BASE_MAX_CLIENTS,
> {
    dispatchers: [Option<(u8, &'a mut dyn ResourceDispatch)>; MAX_RESOURCES],
    count: usize,
    clients: [ClientGen; MAX_CLIENTS],
}

impl<'a, const MAX_RESOURCES: usize, const MAX_CLIENTS: usize>
    Server<'a, MAX_RESOURCES, MAX_CLIENTS>
{
    pub fn new() -> Self {
        Self {
            dispatchers: [const { None }; MAX_RESOURCES],
            count: 0,
            clients: [const {
                ClientGen {
                    task_index: 0,
                    generation: userlib::Gen::DEFAULT,
                    active: false,
                }
            }; MAX_CLIENTS],
        }
    }

    /// Register a resource dispatcher for a given kind byte.
    pub fn register(&mut self, kind: u8, dispatcher: &'a mut dyn ResourceDispatch) {
        if self.count < MAX_RESOURCES {
            self.dispatchers[self.count] = Some((kind, dispatcher));
            self.count += 1;
        }
    }

    pub fn with_dispatcher(
        mut self,
        kind: u8,
        dispatcher: &'a mut dyn ResourceDispatch,
    ) -> Self {
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
        // No free slot — silently ignore (generation tracking is best-effort).
    }

    /// Run the server loop forever, dispatching incoming messages.
    pub fn run(&mut self, buf: &mut [MaybeUninit<u8>]) -> ! {
        loop {
            let msg = userlib::sys_recv_msg_open(buf);

            // Detect client restarts and clean up stale resources.
            self.check_client_generation(msg.sender);

            let (kind, method) = split_opcode(msg.operation);

            let result = self
                .dispatchers
                .iter_mut()
                .find_map(|entry| {
                    let (k, d) = entry.as_mut()?;
                    if *k == kind {
                        Some(d.dispatch(method, &msg))
                    } else {
                        None
                    }
                })
                .unwrap_or(Err(ReplyFaultReason::UndefinedOperation));

            if let Err(e) = result {
                userlib::sys_reply_fault(msg.sender, e);
            }
        }
    }
}
