# IPC System

## Kernel Primitives
The kernel gives us:
- opcode: u16
- message: [u8; 256]
- reply: [u8; 256]
- leases: [Lease; 256]

## Opcode Layout
We split the opcode into `(kind: u8, method: u8)`.
- High byte = resource kind
- Low byte = method ID
- e.g. `(0x11, 0x01)` = File::read

## Resource Model
The IPC framework is fundamentally resource-based. Every resource is defined as a trait:

```rust
#[ipc::resource(arena_size = 10, kind = 0x11)]
pub trait File {
    #[constructor]
    fn open(path: Path) -> Result<Self, OpenError>;

    #[message]
    fn read(&self, offset: u32, #[lease] buf: &mut [u8]) -> Result<usize, ReadError>;

    #[destructor]
    fn delete(self) -> Result<(), DeleteError>;
}
```

The macro generates:
- A server trait with `Meta` injected into every method
- A dispatcher that deserializes messages, manages the arena, and calls handlers
- A client handle type with auto-reconstruction on server death
- An operation enum for dispatch

## Reserved Method IDs
- `0xFF` = implicit destroy (Drop on client handle)
- `0xFE` = transfer ownership (handle forwarding)

---

# Implementation Plan

## Phase 0: Foundations
Small changes to the existing system that later phases build on.

### 0a. Reserve method ID 0xFE for transfer

**Files:** `handle.rs`

Add `pub const TRANSFER_METHOD: u8 = 0xFE;` alongside `IMPLICIT_DESTROY_METHOD`.
Update the method ID overflow check in `parse.rs` to cap at `0xFD` instead of `0xFE`.

### 0b. Arena::transfer()

**Files:** `arena.rs`

```rust
pub fn transfer(&mut self, handle: RawHandle, current_owner: u16, new_owner: u16) -> bool {
    if let Some(entry) = self.map.iter_mut()
        .find(|e| e.occupied && e.key == handle.0 && e.owner == current_owner)
    {
        entry.owner = new_owner;
        true
    } else {
        false
    }
}
```

### 0c. Arena refcount support

**Files:** `arena.rs`

Add optional refcount to `HandleEntry`:

```rust
struct HandleEntry {
    key: u64,
    slot: u8,
    generation: u32,
    occupied: bool,
    owner: u16,
    refcount: u16,    // NEW: 0 = not refcounted (single-owner), >0 = shared
}
```

Add methods:

```rust
/// Increment refcount. Creates a new handle entry pointing to the same slot.
/// Returns the new handle key, or None if the arena map is full.
pub fn clone_handle(&mut self, handle: RawHandle, new_owner: u16) -> Option<RawHandle>

/// Decrement refcount. Removes the handle entry for this key.
/// Returns Some(value) only when the last reference is removed.
pub fn release(&mut self, handle: RawHandle) -> Option<T>
```

`clone_handle` creates a second `HandleEntry` pointing to the same slot index,
with its own key and owner. Both the original and new entry get `refcount` set
(original bumped to 2 on first clone, new entry starts at contribution).

Actually, simpler: put `refcount` on the **Slot**, not the HandleEntry. Multiple
HandleEntry rows can point to the same slot. The slot's refcount tracks how many
entries reference it. `remove()` decrements and only frees when it hits zero.

Revised:

```rust
struct Slot<T> {
    value: Option<T>,
    generation: u32,
    refcount: u16,     // NEW: number of HandleEntry rows pointing here
}
```

- `alloc()`: sets `refcount = 1`
- `clone_handle()`: finds the slot via the source handle, increments `refcount`,
  creates a new HandleEntry pointing to the same slot, returns new RawHandle
- `remove()`: decrements `refcount`. Only takes the value when `refcount == 0`.
  Always marks the HandleEntry as unoccupied.
- `remove_by_owner()`: same — only takes value on last ref.

### 0d. Infer arena_size = 0 when no constructors

**Files:** `parse.rs`

If the trait has no `#[constructor]` methods, `arena_size` becomes optional and
defaults to 0. If someone explicitly sets `arena_size > 0` on a constructor-less
trait, emit a compile error: "traits without constructors cannot have an arena."

This distinguishes **interfaces** (no constructor, no arena) from **concrete
resources** (has constructors, has arena).

### 0e. Transferable trait

**Files:** `lib.rs`

```rust
/// Implemented by every generated client handle. Enables `#[handle(move)]`.
pub trait Transferable {
    /// Transfer ownership of this handle to `new_owner`.
    /// Sends a 0xFE message to the handle's server, then returns the raw handle.
    /// Consumes self.
    fn transfer_to(self, new_owner: userlib::TaskId) -> Result<RawHandle, Error>;
}

/// Implemented by every generated client handle for refcounted resources.
/// Enables `#[handle(clone)]`.
pub trait Cloneable {
    /// Clone this handle for `new_owner`.
    /// Sends a 0xFD message to the handle's server, returns a new raw handle.
    /// Does NOT consume self.
    fn clone_for(&self, new_owner: userlib::TaskId) -> Result<RawHandle, Error>;
}
```

Reserve `0xFD` for clone as well:
```rust
pub const CLONE_METHOD: u8 = 0xFD;
```

Method ID cap becomes `0xFC`.

---

## Phase 1: Handle Forwarding (#[handle(move)] and #[handle(clone)])

### 1a. Parse #[handle(move)] and #[handle(clone)]

**Files:** `parse.rs`

Add to `ParsedParam`:

```rust
pub enum HandleMode {
    Move,
    Clone,
}

pub struct ParsedParam {
    pub name: Ident,
    pub ty: Type,
    pub is_lease: bool,
    pub lease_mutable: bool,
    pub handle_mode: Option<HandleMode>,   // NEW
}
```

Parse `#[handle(move)]` and `#[handle(clone)]` attributes on parameters, similar
to how `#[lease]` is parsed today. Validation:
- `#[handle(...)]` and `#[lease]` are mutually exclusive
- `#[handle(move)]` params must not be `&self` or `&mut self`

### 1b. Server-side codegen for handle params

**Files:** `codegen_server.rs`

For `#[handle(move)]` and `#[handle(clone)]` params:
- **Server trait**: replace the user's type with `ipc::RawHandle`
- **Deserialization**: deserialize as `ipc::RawHandle` (just a u64 in the args tuple)
- No other server-side changes — the server receives a raw handle and does
  whatever it wants with it

### 1c. Client-side codegen for handle params

**Files:** `codegen_client.rs`

For `#[handle(move)]` params:
- **Method signature**: keep the user's written type, but add a trait bound:
  the param type must impl `ipc::Transferable`
- **Before serializing**: call `param.transfer_to(self.server.get())?` to get
  a `RawHandle`. Serialize that `RawHandle` in the args tuple.
- The original handle is consumed (moved into `transfer_to`).

For `#[handle(clone)]` params:
- **Method signature**: take `&param_type` (reference), require `ipc::Cloneable`
- **Before serializing**: call `param.clone_for(self.server.get())?` to get
  a `RawHandle`. Serialize that `RawHandle`.
- The original handle is NOT consumed.

### 1d. Generate Transferable impl for every handle

**Files:** `codegen_client.rs`

Every generated `XxxHandle<S, E>` gets:

```rust
impl<S: XxxServer, E> ipc::Transferable for XxxHandle<S, E> {
    fn transfer_to(self, new_owner: userlib::TaskId) -> Result<ipc::RawHandle, ipc::Error> {
        self.destroyed.set(true); // prevent Drop from sending 0xFF
        let args: (ipc::RawHandle, u16) = (self.handle.get(), new_owner.task_index());
        let mut argbuffer = [0u8; <(ipc::RawHandle, u16) as hubpack::SerializedSize>::MAX_SIZE];
        let n = hubpack::serialize(&mut argbuffer, &args).unwrap_or(argbuffer.len());
        let mut retbuffer = [0u8; <Result<(), ipc::Error> as hubpack::SerializedSize>::MAX_SIZE];
        let mut leases = [];
        let (rc, len) = userlib::sys_send(
            self.server.get(),
            ipc::opcode(KIND, ipc::TRANSFER_METHOD),
            &argbuffer[..n],
            &mut retbuffer,
            &mut leases,
        ).map_err(|_| ipc::Error::ServerDied)?;
        // deserialize Result<(), ipc::Error>, propagate errors
        // on success, return self.handle.get()
        Ok(self.handle.get())
    }
}
```

### 1e. Generate Cloneable impl for refcounted resources

**Files:** `codegen_client.rs`

Only generated when the resource has `clone = refcount`. Similar to Transferable
but sends `0xFD`, doesn't consume self, returns the NEW handle key from the
server reply.

### 1f. Dispatcher handles 0xFE and 0xFD

**Files:** `codegen_server.rs`

In the generated `ResourceDispatch::dispatch`, alongside the 0xFF handling:

```rust
if method_id == ipc::TRANSFER_METHOD {
    let (handle, new_owner) = deserialize::<(RawHandle, u16)>(msg_data)?;
    let ok = self.arena.transfer(handle, msg.sender.task_index(), new_owner);
    // serialize Result<(), ipc::Error>, reply
}

if method_id == ipc::CLONE_METHOD {
    let (handle, new_owner) = deserialize::<(RawHandle, u16)>(msg_data)?;
    match self.arena.clone_handle(handle, new_owner) {
        Some(new_handle) => // reply Ok(new_handle)
        None => // reply Err(ArenaFull)
    }
}
```

### 1g. Make ctor args optional for transferred handles

**Files:** `codegen_client.rs`

Change the generated handle struct:

```rust
pub struct XxxHandle<S, E> {
    server: Cell<TaskId>,
    handle: Cell<RawHandle>,
    ctor: Option<XxxCtorArgs>,    // was: XxxCtorArgs
    destroyed: Cell<bool>,
    ...
}
```

`reconstruct()` checks `self.ctor`:
- `Some(args)` -> reconstruct as today
- `None` -> return `Err(ipc::Error::ServerDied)` (can't reconstruct a transferred handle)

Add a `from_raw(handle: RawHandle) -> Self` associated function that creates a
handle with `ctor: None`. This is used by server implementations that receive a
`RawHandle` and want to wrap it in a typed client handle.

---

## Phase 2: Trait Inheritance

### 2a. Parse trait supertraits

**Files:** `parse.rs`

When the macro sees `trait Sdmmc: Storage`, extract the supertrait name(s).
Store them in `ResourceAttr`:

```rust
pub struct ResourceAttr {
    pub arena_size: Option<usize>,  // None for interface-only traits
    pub kind: u8,
    pub supertraits: Vec<Ident>,    // NEW
}
```

### 2b. Inherit methods from supertraits

This is the hard part. The macro expanding `Sdmmc` doesn't have access to
`Storage`'s parsed methods — they were expanded in a different crate.

**Solution: trait method registry via const items.**

When the `Storage` macro expands, in addition to the server trait / client / etc,
it emits a const item encoding the method table:

```rust
// Generated by #[ipc::resource] on Storage
pub const STORAGE_IPC_METHODS: &[ipc::MethodDescriptor] = &[
    ipc::MethodDescriptor {
        name: "read",
        method_id: 0,
        kind: MethodKind::Message,
        // ... enough info to generate dispatch arms
    },
    // ...
];
```

When `Sdmmc: Storage` is expanded, the macro:
1. Generates dispatch arms for Sdmmc's own methods (starting IDs after Storage's)
2. At runtime, the Sdmmc dispatcher also handles Storage method IDs by
   delegating to the same handler impls (since `T: Sdmmc` implies `T: Storage`
   via Rust's trait inheritance)

**Alternative (simpler): no cross-crate inheritance at compile time.**

Instead, trait inheritance is purely a client-side concern. The `Sdmmc` server
implements both `Storage` and `Sdmmc` methods directly. The `Sdmmc` kind handles
all the method IDs. The supertrait just tells the client codegen that an
`SdmmcHandle` can be used where `impl Storage` is expected (Phase 3).

The server author manually ensures method IDs don't collide by putting Storage
methods first in the Sdmmc trait:

```rust
#[ipc::resource(arena_size = 1, kind = 0x11)]
pub trait Sdmmc {
    // Storage methods (must match Storage's method IDs)
    #[message]
    fn read(&self, offset: u64, #[lease] into: &mut [u8]) -> Result<usize, ReadErr>;

    // Sdmmc-specific methods
    #[constructor]
    fn open() -> Result<Self, AlreadyOpenError>;
}
```

This is less magical but more explicit. The `#[ipc::implements(Storage)]`
attribute could verify at compile time that the method signatures match and
IDs are compatible.

**Recommendation: start with the simpler approach.** Cross-crate method
inheritance via const registries is complex. Start with `implements` verification
and client-side polymorphism. Revisit if the boilerplate becomes painful.

### 2c. implements attribute

**Files:** new attribute macro or extension of `resource`

```rust
#[ipc::resource(arena_size = 1, kind = 0x11, implements = [Storage])]
pub trait Sdmmc {
    // Must include all Storage methods with matching signatures and IDs
    #[message]
    fn read(&self, offset: u64, #[lease] into: &mut [u8]) -> Result<usize, ReadErr>;

    #[constructor]
    fn open() -> Result<Self, AlreadyOpenError>;
}
```

The macro verifies method compatibility at compile time if possible (same crate),
or defers to a runtime/link-time check if cross-crate.

---

## Phase 3: Dynamic Handles (impl Trait)

### 3a. DynHandle type

**Files:** new file `dyn_handle.rs` in `ipc/src/`

```rust
/// A type-erased handle that can point to any server implementing a given
/// interface. Carries the server TaskId, resource kind, and raw handle.
pub struct DynHandle {
    pub server: TaskId,
    pub kind: u8,
    pub handle: RawHandle,
}
```

This is what gets serialized on the wire when a method takes `impl Storage`.
It's 11 bytes (TaskId + u8 + u64).

### 3b. Into<DynHandle> for concrete handles

**Files:** `codegen_client.rs`

Every generated handle that `implements` an interface gets a conversion:

```rust
impl<S: SdmmcServer, E> From<SdmmcHandle<S, E>> for ipc::DynHandle {
    fn from(h: SdmmcHandle<S, E>) -> ipc::DynHandle {
        ipc::DynHandle {
            server: h.server.get(),
            kind: KIND,
            handle: h.handle.get(),
        }
    }
}
```

### 3c. Client-side dynamic dispatch

**Files:** `codegen_client.rs` or new file

For each interface trait (arena_size = 0, no constructors), generate a dynamic
client wrapper:

```rust
/// Dynamic client for any server implementing Storage.
pub struct StorageDyn {
    inner: ipc::DynHandle,
}

impl StorageDyn {
    pub fn read(&self, offset: u64, into: &mut [u8]) -> Result<usize, ipc::Error> {
        // Build opcode from self.inner.kind (not a static KIND)
        // and Storage::read's method_id
        let opcode = ipc::opcode(self.inner.kind, 0x00);
        // serialize, sys_send to self.inner.server, deserialize
    }
}
```

### 3d. impl Trait in method signatures

**Files:** `parse.rs`, `codegen_client.rs`, `codegen_server.rs`

When the macro sees `impl Storage` as a parameter type:
- **Parse**: recognize it as a dynamic handle parameter. Store the trait name.
- **Client codegen**: the parameter type becomes `impl Into<ipc::DynHandle>`.
  Before serializing, convert to `DynHandle`. For `#[handle(move)]`, call
  `transfer_to` first, then build the `DynHandle`. Serialize the `DynHandle`
  in the args tuple.
- **Server codegen**: the parameter type becomes `ipc::DynHandle` in the server
  trait. Deserialized as `DynHandle`.

When the macro sees `impl Storage` as a return type (with `constructs`):
- **Server codegen**: the server returns a `RawHandle` as usual. The dispatcher
  wraps it into a `DynHandle` using the server's own TaskId and kind.
- **Client codegen**: deserialize a `DynHandle` from the reply. Wrap it in the
  dynamic client (`StorageDyn`).

---

## Phase 4: Cross-Arena Construction (#[message(constructs = X)])

### 4a. Parse constructs attribute

**Files:** `parse.rs`

On `#[message]` methods, parse an optional `constructs = TraitName`:

```rust
pub struct ParsedMethod {
    ...
    pub constructs: Option<Ident>,  // NEW
}
```

Validation:
- Only allowed on `#[message]` methods (not constructors/destructors)
- The return type must be `Self`, `Option<Self>`, or `Result<Self, E>` —
  where `Self` here refers to the *constructed* resource, not the parent

### 4b. Server-side codegen for constructs

**Files:** `codegen_server.rs`

A method with `constructs = FileSystem` allocates in the **FileSystem** arena
instead of its own. The dispatcher needs access to both arenas.

**Approach**: the generated dispatcher for `FileSystemRegistry` takes a
reference to the `FileSystem` dispatcher's arena:

```rust
pub struct FileSystemRegistryDispatcher<T: FileSystemRegistry> {
    pub arena: ipc::Arena<T, REGISTRY_SIZE>,
    pub fs_arena: *mut ipc::Arena<FileSystemImpl, FS_SIZE>,  // borrowed
}
```

This is messy with raw pointers. Better approach: the `constructs` method
returns the constructed value, and the **server main loop** is responsible for
inserting it into the right arena. This means the dispatch can't be fully
self-contained — the server needs a post-dispatch hook.

**Alternative**: the server trait method returns the value, and the macro
generates a wrapper that allocates it:

```rust
// Generated server trait
trait FileSystemRegistry: Sized {
    fn open(&mut self, meta: Meta, name: [u8; 16]) -> Option<FileSystemImpl>;
}

// Generated dispatch — the dispatcher is given a closure/trait that can
// allocate in the FileSystem arena:
fn dispatch(&mut self, method_id: u8, msg: &Message, allocator: &mut dyn FnMut(FileSystemImpl) -> Option<RawHandle>) {
    ...
    let value = resource.open(meta, name);
    if let Some(v) = value {
        let handle = allocator(v);
        // reply with handle
    }
}
```

This needs more design work. Defer details to implementation time, but the
key constraint is: both arenas must live in the same server process, and the
server main function wires them together.

### 4c. Client-side codegen for constructs

**Files:** `codegen_client.rs`

The client method returns a handle to the *constructed* resource, not the
parent. The return type in the client becomes `Result<FileSystemHandle, ...>`
even though the method is on `FileSystemRegistryHandle`.

The client deserializes a `RawHandle` from the reply and wraps it in a
`FileSystemHandle::from_raw(handle)`.

---

## Phase 5: (removed)

Hierarchical ownership (`parent = X`) was removed. Resource cleanup of
child resources is handled by the parent resource's `Drop` implementation
or explicit destructor logic in the server trait. This keeps the arena
simple and puts relationship knowledge where it belongs — in the server
author's code.

---

## Phase 6: clone = refcount

### 6a. Parse clone attribute

**Files:** `parse.rs`

```rust
pub enum CloneMode {
    Refcount,
}

pub struct ResourceAttr {
    ...
    pub clone_mode: Option<CloneMode>,
}
```

Usage: `#[ipc::resource(arena_size = 16, kind = 0x13, clone = refcount)]`

### 6b. Validation

- If `clone = refcount`, the trait must NOT have any `#[destructor]` methods.
  Emit a compile error if it does: "refcounted resources cannot have explicit
  destructors; they are freed when the last reference is released."
- Implicit destroy (Drop, 0xFF) still works — it decrements the refcount and
  frees when it hits zero.

### 6c. Dispatcher changes

For refcounted resources, the 0xFF handler calls `arena.release(handle)` instead
of `arena.remove(handle)`. `release` decrements the refcount and only drops the
value when it hits zero.

### 6d. Client Drop codegen

No change needed — Drop still sends 0xFF. The server handles the refcount
semantics. The client doesn't know or care whether the resource is refcounted.

---

## Phase Summary

| Phase | Feature                        | Depends On | Complexity |
|-------|--------------------------------|------------|------------|
| 0     | Foundations (arena, traits)     | —          | Small      |
| 1     | #[handle(move/clone)]          | Phase 0    | Medium     |
| 2     | Trait inheritance               | Phase 0    | Medium     |
| 3     | Dynamic handles (impl Trait)   | Phase 1, 2 | Large      |
| 4     | Cross-arena construction       | Phase 1    | Medium     |
| 5     | Hierarchical ownership         | Phase 4    | Medium     |
| 6     | clone = refcount               | Phase 0    | Small      |

Phases 0 and 6 can be done independently. Phase 1 requires Phase 0.
Phases 2 and 4 can proceed in parallel after Phase 1. Phase 3 needs both
1 and 2. Phase 5 needs Phase 4.

## Target API (end state)

```rust
// Pure interface — no constructor, no arena
#[ipc::resource(kind = 0x10)]
pub trait Storage {
    #[message]
    fn read(&self, offset: u64, #[lease] into: &mut [u8]) -> Result<usize, ReadErr>;

    #[message]
    fn write(&self, offset: u64, #[lease] data: &[u8]) -> Result<usize, WriteErr>;
}

// Concrete resource implementing Storage
#[ipc::resource(arena_size = 1, kind = 0x11, implements = [Storage])]
pub trait Sdmmc {
    #[message]
    fn read(&self, offset: u64, #[lease] into: &mut [u8]) -> Result<usize, ReadErr>;

    #[message]
    fn write(&self, offset: u64, #[lease] data: &[u8]) -> Result<usize, WriteErr>;

    #[constructor]
    fn open() -> Result<Self, AlreadyOpenError>;
}

// Refcounted resource with dynamic handle params
#[ipc::resource(arena_size = 16, kind = 0x13, clone = refcount)]
pub trait FileSystem {
    #[constructor]
    fn mount(#[handle(move)] storage: impl Storage) -> Result<Self, MountError>;

    #[message(constructs = File)]
    fn open(&self, path: [u8; 64]) -> Result<impl File, OpenError>;
}

// Child resource — cleanup handled by FileSystem's Drop/destructor
#[ipc::resource(arena_size = 128, kind = 0x14)]
pub trait File: Storage {
    #[message]
    fn read(&self, offset: u64, #[lease] into: &mut [u8]) -> Result<usize, ReadErr>;

    #[message]
    fn write(&self, offset: u64, #[lease] data: &[u8]) -> Result<usize, WriteErr>;
}
```
