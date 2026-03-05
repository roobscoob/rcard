# Kernel
The kernel gives us:
opcode: u16
message: [u8; 256]
reply: [u8; 256]
leases: [Lease; 256]

# IPC Framework
The IPC framework is fundementally resource-based.
The IPC framework is responsible for the **lifetime of resources**.

we split the opcode into (u8, u8) - which are allocated.

The most significant byte is the "resource kind"
the least significant byte is the "method id"

so (0x01, 0x01) might be File::get

For example - you might define a `File` resource.

```rs
// library crate (that uses the ipc macro)
#[ipc::resource(arena_size = 10, kind = 0x11)]
pub trait File {
    #[constructor]
    pub fn get_or_create(path: Path, #[lease] init_data: &[u8]) -> Self;

    #[constructor]
    pub fn get(path: Path) -> Self;

    #[message]
    pub fn read(&self, offset: u32, #[lease] result: &mut [u8]) -> Result<usize, self::Error>;

    #[message]
    pub fn truncate(&self, new_length: u32) -> Result<usize, self::Error>;

    #[destructor]
    pub fn delete(self) -> Result<(), self::Error>;
}

// the server crate will import the library crate...
impl File for FileHandler;

fn main() {
    ipc::Server::new()
        .with_handler(FileHandler)
        .listen();

// then the client will also implement the library crate, but only library::client:
use library::client::{FileHandle};

FileHandle::<16>::get_or_create()
}
```

