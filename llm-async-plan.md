# Async IPC Implementation Plan

## Context

The IPC system is fully synchronous: `sys_send` blocks until `sys_reply`, and notification handlers are synchronous callbacks in the server loop. This prevents tasks from doing concurrent work (e.g., a compositor serving framebuffers while awaiting a display notification, or a client issuing multiple IPC calls concurrently).

This plan adds async support on top of the existing Hubris syscalls — no kernel changes. The sync path stays intact; async is opt-in per task.

## Design Summary

- **`#[ipc::main]`** attribute macro on `async fn main()` — generates a `-> !` wrapper with executor loop, server infra, and stack-local future storage. Replaces `ipc::server!` for tasks that want async.
- **`ipc::notification!(group).await`** — replaces `#[ipc::notification_handler]`.
- **`ipc::irq!(peripheral).await`** — for hardware interrupts.
- **Async IPC methods** — sync preamble reads leases, returns a future. Client sees `async fn`.
- **All futures on the stack** — `MaybeUninit<_>` with type inference, no TAIT, no alloc. Sound because `-> !` stack never unwinds.

---

## 1. New Runtime Files

All in `firmware/modules/ipc/src/`.

### `executor.rs` — ExecutorState, wakers, EXEC_PTR

```rust
pub struct ExecutorState {
    pending_notifications: Cell<u32>,  // bits from sys_recv_open not yet consumed
    main_woken: Cell<bool>,
    task_woken: Cell<u32>,             // bitmask: spawned tasks needing poll
    async_slot_woken: Cell<u32>,       // bitmask: handler slots needing poll
    pub bit_pool: BitPool,
}
```

- Global `EXEC_PTR: Cell<*const ExecutorState>` — set once before the loop, accessed via `pub fn exec_ref() -> &'static ExecutorState`.
- `RawWaker` vtable: data encodes `WakerKind` (Main, Task(n), AsyncSlot(n), NotificationBit(bit)). `wake()` calls `exec_ref()` and sets the appropriate ready flag. Single-threaded, no atomics.
- `deliver_notifications(bits: u32)` — `pending_notifications |= bits`, then for each bit that has an interested future, set the woken flag.
- `consume_notification_bit(bit: u32)` — clears from pending.

### `bit_pool.rs` — Notification bit allocator

```rust
pub struct BitPool { available: Cell<u32> }
```

Initialized with `!irq_bits & 0x7FFF_FFFF` (excludes IRQ bits and bit 31 for reactor). Methods: `alloc() -> Option<u8>`, `free(bit: u8)`.

### `futures.rs` — IrqFuture, NotificationFuture, NotificationBitFuture

1. **`IrqFuture { bit: u32 }`** — `ipc::irq!(peripheral)` resolves to a compile-time notification bit. On poll: check `pending_notifications & bit`, consume, re-enable IRQ, return `Ready(())`. Otherwise register waker, return `Pending`.

2. **`NotificationFuture { group_id: u16 }`** — `ipc::notification!(group)` expands to this. Shares bit 31 (reactor). On poll: call `Reactor::try_pull_group(group_id)`. Ready if notification available, Pending otherwise.

3. **`NotificationBitFuture { bit: u8 }`** — used by async IPC client methods. Waits on a specific dynamically-allocated notification bit. Same pattern as IrqFuture but without IRQ re-enable.

### `pending_results.rs` — Server-side async result storage

```rust
pub struct PendingResults<const N: usize> {
    slots: [PendingSlot; N],
}
struct PendingSlot {
    occupied: Cell<bool>,
    client_task: Cell<u16>,
    reply_bit: Cell<u8>,
    result_buf: UnsafeCell<[u8; 256]>,
    result_len: Cell<u16>,
    result_code: Cell<u8>,
}
```

- `store(client_task, reply_bit, result_code, data)` — serialize into slot, `sys_post(client_tid, 1 << reply_bit)`
- `take_and_reply(sender, reply_bit)` — find slot by `(sender.task_index(), reply_bit)`, reply with stored data, free slot
- `remove_by_client(task_index)` — cleanup on client death

---

## 2. Changes to Existing Runtime

### `handle.rs` — New reserved method IDs

```rust
pub const TAKE_RESULT_METHOD: u8 = 0xF7;  // client retrieves async result
pub const ASYNC_SUBMIT_FLAG: u8 = 0x80;   // high bit signals async submit
```

Method ID space: user methods 0x00–0x76, async submit uses `method | 0x80` = 0x80–0xF6, 0xF7 = TAKE_RESULT, 0xF8–0xFF = existing protocol methods. The macro validates async methods have IDs < 0x77.

### `server.rs` — Make dispatch_message callable from executor

Add a public method so `#[ipc::main]`'s executor loop can call it:

```rust
pub fn dispatch_message_from(&mut self, msg: &Message<'_>) {
    self.dispatch_message(msg);
}
```

The `#[ipc::main]` generated loop intercepts `TAKE_RESULT_METHOD` and async submit (`method & 0x80`) *before* calling this. Sync messages pass through unchanged.

### `call.rs` — Async client helpers

Add to `IpcCall`:
- `send_async_submit(&mut self, reply_bit: u8) -> Result<(), Error>` — sends with `method_id | 0x80`, prepends `reply_bit` to args, reads ACK byte from reply
- Free function `take_result(server, kind, reply_bit) -> Result<(ResponseCode, &[u8]), Error>` — sends `TAKE_RESULT_METHOD` with `reply_bit` arg

### `lib.rs` — Wire new modules

```rust
pub mod executor;
pub mod bit_pool;
pub mod futures;
pub mod pending_results;
```

---

## 3. Macro Changes

All in `firmware/modules/ipc/macros/src/`.

### `parse.rs` — Add AsyncMessage method kind

```rust
pub enum MethodKind {
    Constructor,
    Message,
    StaticMessage,
    Destructor,
    AsyncMessage,  // NEW: #[async_message] or #[async_message(capacity = N)]
}
```

In `classify_method()`, recognize `#[async_message]` attribute. Parse optional `capacity = N` (default 1). Store capacity in `ParsedMethod`.

Validation: async methods must have a receiver (`&mut self`), cannot be constructors or destructors.

### New: `ipc_main.rs` — `#[ipc::main]` attribute macro

**Input syntax:**
```rust
#[ipc::main(
    serve(FrameBuffer: FrameBufferResource),
    tasks(present_loop),
)]
async fn main() { ... }
```

**Parsing:** New struct `IpcMainAttr` with `entries: Vec<ServerEntry>`, `tasks: Vec<Ident>`. Reuses `ServerEntry` from `server_macro.rs`.

**Code generation:** Generates a `#[export_name = "main"] fn __ipc_main() -> !` containing:

1. **Server infra** — reuse `gen_priority_fn()`, `gen_acl_fn()`, `gen_self_task_index()` (extract from `server_macro.rs` into shared functions). Generate arena declarations, dispatcher wiring, `Server::<N>` construction — same code `ipc::server!` generates.

2. **Executor state** — `let __exec = ExecutorState::new(BitPool::new(irq_bits));` + `set_exec(&__exec)`.

3. **Future storage** (all `MaybeUninit<_>` on stack):
   - Main future: `let mut __main_fut = MaybeUninit::uninit(); __main_fut.write(async { /* user body */ });`
   - Spawned tasks: `let mut __task_N = MaybeUninit::uninit(); __task_N.write(task_fn());`
   - Async handler slots: N slots per async method per resource (from `#[async_message(capacity = N)]`)

4. **Executor loop:**
   ```
   loop {
       // Phase 1: Poll main (if woken and not completed)
       // Phase 2: Poll spawned tasks (if woken)
       // Phase 3: Poll async handler slots (if occupied and woken)
       // Phase 4: sys_recv_open(buf, notification_mask)
       //   Notification → deliver_notifications, set woken flags
       //   Message →
       //     if method == TAKE_RESULT_METHOD → pending_results.take_and_reply
       //     if method & 0x80 → inline async dispatch (per-resource)
       //     else → server.dispatch_message_from(msg)
   }
   ```

5. **Inline async dispatch** — for each resource with async methods, generate:
   ```rust
   if kind == FB_KIND && (method & 0x80) != 0 {
       let real_method = method & 0x7F;
       match real_method {
           0x02 => { // e.g., process()
               // Deserialize args, bind leases
               // Find free slot
               if __async_fb_process_0_meta.is_none() {
                   let future = resource.process(meta, lease);
                   reply.reply_ok(&[0u8]); // ACK
                   __async_fb_process_0.write(future);
                   __async_fb_process_0_meta = Some(AsyncSlotMeta { client_task, reply_bit });
                   __exec.wake_async_slot(0); // ensure initial poll
               } else { reply.reply_ok(&[1u8]); } // rejected
           }
           _ => reply.reply_error(MALFORMED_MESSAGE, &[]);
       }
   }
   ```

6. **Client death cleanup** — in the generation-check path, drop in-flight async futures and free PendingResults for the dead client.

### `server/dispatcher.rs` — Async method awareness

For `#[async_message]` methods in the trait, the **server trait** generated by `gen_server_trait()` declares:
```rust
fn process(&mut self, meta: Meta, data: LeaseBorrow<Read>) -> impl Future<Output = Result<T, E>>;
```

The sync dispatcher ignores async methods (they're dispatched inline in `#[ipc::main]`, not through `ResourceDispatch::dispatch`). The op enum still includes them for method ID assignment.

### `client/methods.rs` — Async client methods

For `#[async_message]` methods, generate:
```rust
pub async fn process(&self, data: &[u8]) -> Result<T, Error> {
    let reply_bit = ipc::executor::exec_ref().bit_pool.alloc()
        .ok_or(ipc::Error::TooManyConcurrentOps)?;
    // Submit
    let mut call = IpcCall::new(self.server(), KIND, METHOD_ID | 0x80);
    call.push_arg(&reply_bit);
    call.push_arg(&data_arg);
    call.add_read_lease(data);
    let (rc, _, _) = call.send_raw()?;
    if rc != SUCCESS { exec_ref().bit_pool.free(reply_bit); return Err(Error::Rejected); }
    // Await notification
    ipc::futures::NotificationBitFuture::new(reply_bit).await;
    // Take result
    let result = ipc::call::take_result(self.server(), KIND, reply_bit)?;
    exec_ref().bit_pool.free(reply_bit);
    deserialize(result)
}
```

### `lib.rs` — New entry point

```rust
#[proc_macro_attribute]
pub fn main(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(attr as ipc_main::IpcMainAttr);
    let func = parse_macro_input!(item as syn::ItemFn);
    ipc_main::gen_ipc_main(&input, &func).into()
}
```

Also add `ipc::notification!` and `ipc::irq!` proc macros (or declarative macros exported from the runtime crate).

---

## 4. Wire Protocol

Three fast synchronous IPCs per async call:

1. **Submit** — client sends `[reply_bit, ...args]` with leases. Method ID has `| 0x80`. Server runs sync preamble (reads leases), replies `[0]` (accepted) or `[1]` (rejected/full). Client unblocks.

2. **Notify** — executor polls the future. On completion, result serialized into `PendingSlot`, `sys_post(client, 1 << reply_bit)`.

3. **Take** — client sends `TAKE_RESULT_METHOD` (0xF7) with `reply_bit` arg. Server replies with stored result.

---

## 5. Implementation Order

### Phase 1: Runtime foundations (no breaking changes)
1. `bit_pool.rs` — pure data structure
2. `executor.rs` — ExecutorState, exec_ref, waker vtable
3. `futures.rs` — IrqFuture, NotificationBitFuture
4. `pending_results.rs` — PendingResults, PendingSlot
5. Add `TAKE_RESULT_METHOD`, `ASYNC_SUBMIT_FLAG` to `handle.rs`
6. Add async helpers to `call.rs`
7. Make `dispatch_message` publicly callable in `server.rs`
8. Wire modules in `lib.rs`

### Phase 2: `#[ipc::main]` for sync-only servers
9. Extract shared codegen from `server_macro.rs` (priority, ACL, self_task_index, arena/dispatcher/registration)
10. New `ipc_main.rs` — parse attributes, generate `-> !` wrapper with executor loop wrapping `Server::dispatch_message_from`
11. Add `#[ipc::main]` entry point in macro `lib.rs`
12. **Test:** convert `sysmodule/time` (simple, no notifications) to `#[ipc::main] async fn main()` — sync behavior only

### Phase 3: Notification futures
13. Add `NotificationFuture` to `futures.rs`
14. Add `ipc::notification!` macro
15. Wire notification delivery in executor (bit 31 → wake NotificationFuture, pull from Reactor)
16. **Test:** convert `sysmodule/compositor` to `ipc::notification!(present).await`

### Phase 4: IRQ futures
17. Add `ipc::irq!` macro (resolve peripheral → bit from build data)
18. Wire IRQ delivery in executor
19. **Test:** convert a driver task with hardware IRQs

### Phase 5: Async IPC methods (server side)
20. Add `#[async_message]` to `parse.rs`
21. Generate async method signatures in server trait
22. Generate inline async dispatch in `#[ipc::main]` expansion
23. Generate `MaybeUninit` slot storage per async method
24. Wire PendingResults + TAKE_RESULT handling
25. Client death cleanup for async slots
26. **Test:** hand-write one async method, verify submit/notify/take cycle

### Phase 6: Async IPC methods (client side)
27. Generate `async fn` client stubs for `#[async_message]` methods
28. BitPool alloc/free in generated code
29. **Test:** end-to-end async IPC client → server → client

### Phase 7: Migration
30. Keep `ipc::server!` working (no deprecation needed yet)
31. Migrate tasks that benefit from async
32. Verify sync-only tasks (USB) work unchanged with `#[ipc::main]`

---

## 6. Critical Files

**New files:**
- `firmware/modules/ipc/src/executor.rs`
- `firmware/modules/ipc/src/bit_pool.rs`
- `firmware/modules/ipc/src/futures.rs`
- `firmware/modules/ipc/src/pending_results.rs`
- `firmware/modules/ipc/macros/src/ipc_main.rs`

**Modified files:**
- [handle.rs](firmware/modules/ipc/src/handle.rs) — TAKE_RESULT_METHOD, ASYNC_SUBMIT_FLAG
- [server.rs](firmware/modules/ipc/src/server.rs) — public dispatch_message
- [call.rs](firmware/modules/ipc/src/call.rs) — async submit/take helpers
- [lib.rs (runtime)](firmware/modules/ipc/src/lib.rs) — new module exports
- [lib.rs (macros)](firmware/modules/ipc/macros/src/lib.rs) — `#[ipc::main]`, `notification!`, `irq!` entry points
- [server_macro.rs](firmware/modules/ipc/macros/src/server_macro.rs) — extract shared codegen for reuse
- [parse.rs](firmware/modules/ipc/macros/src/parse.rs) — AsyncMessage variant
- [server/dispatcher.rs](firmware/modules/ipc/macros/src/server/dispatcher.rs) — async method in server trait
- [client/methods.rs](firmware/modules/ipc/macros/src/client/methods.rs) — async fn client stubs

---

## 7. Verification

- Compositor: `#[ipc::main(serve(FrameBuffer: FrameBufferResource), tasks(present_loop))]` — display renders via `ipc::notification!(present).await`
- USB: `#[ipc::main(serve(UsbBus: UsbBusResource, UsbEndpoint: UsbEndpointResource))]` — sync only, works unchanged
- Pure client: `#[ipc::main] async fn main()` that completes — executor idles on `sys_recv_open`
- Async IPC end-to-end: client `.await`, server processes, client gets result
- `warnings = "deny"` clippy passes
- ARM build + Renode emulator test
