#![no_std]

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize, hubpack::SerializedSize, Debug)]
pub enum RegistryError {
    AlreadyExists,
    NotFound,
    RegistryFull,
}

#[derive(serde::Serialize, serde::Deserialize, hubpack::SerializedSize, Debug)]
pub enum FileSystemError {
    CorruptFilesystem,
    TooManyFilesystems,
    StorageError,
    InvalidFs,
}

#[derive(serde::Serialize, serde::Deserialize, hubpack::SerializedSize, Debug)]
pub enum FsError {
    NotFound,
    InvalidFs,
    NoSpace,
    IsDirectory,
    NotDirectory,
    NotEmpty,
    NameTooLong,
    Io,
}

#[derive(serde::Serialize, serde::Deserialize, hubpack::SerializedSize, Debug)]
pub enum OpenError {
    NotFound,
    InvalidFs,
    NoSpace,
    IsDirectory,
    TooManyOpenFiles,
    Io,
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, hubpack::SerializedSize,
)]
pub struct FileSystemStats {
    pub total_blocks: u32,
    pub free_blocks: u32,
    pub block_size: u16,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, hubpack::SerializedSize,
)]
pub enum EntryType {
    File,
    Directory,
}

/// A directory entry returned by `Folder::next`.
///
/// `name` is a null-padded UTF-8 filename (up to 31 bytes + null).
/// When `name_len == 0`, the directory listing is exhausted.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, hubpack::SerializedSize)]
pub struct DirEntry {
    pub name: [u8; 32],
    pub name_len: u8,
    pub entry_type: EntryType,
    pub size: u32,
}

impl DirEntry {
    pub const EMPTY: Self = Self {
        name: [0; 32],
        name_len: 0,
        entry_type: EntryType::File,
        size: 0,
    };

    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }

    pub fn is_end(&self) -> bool {
        self.name_len == 0
    }
}

// ---------------------------------------------------------------------------
// FileOffset newtype
// ---------------------------------------------------------------------------

#[derive(
    Clone, Copy, Debug, serde::Serialize, serde::Deserialize, hubpack::SerializedSize,
)]
pub struct FileOffset(u32);

impl FileOffset {
    pub fn new(v: u32) -> Option<Self> {
        (v <= i32::MAX as u32).then_some(Self(v))
    }

    pub fn get(self) -> u32 {
        self.0
    }

    pub fn as_i32(self) -> i32 {
        self.0 as i32
    }
}

// ---------------------------------------------------------------------------
// Resource traits
// ---------------------------------------------------------------------------

/// A filesystem registry — maps names to mounted filesystem IDs.
#[ipc::resource(arena_size = 4, kind = 0x14)]
pub trait FileSystemRegistry {
    #[constructor]
    fn global() -> Self;

    #[message]
    fn register(&self, name: [u8; 16], fs_id: u8) -> Result<(), RegistryError>;
}

/// A mounted filesystem backed by a storage device.
#[ipc::resource(arena_size = 4, kind = 0x12, clone = refcount)]
pub trait FileSystem {
    #[constructor]
    fn mount(#[handle(move)] storage: impl Storage) -> Result<Self, FileSystemError>;

    #[constructor]
    fn lookup(#[handle(clone)] registry: &impl FileSystemRegistry, name: [u8; 16]) -> Option<Self>;

    #[constructor]
    fn format(#[handle(move)] storage: impl Storage) -> Result<Self, FileSystemError>;

    #[message]
    fn stat(&self) -> FileSystemStats;
}

/// An open file within a mounted filesystem.
#[ipc::resource(arena_size = 4, kind = 0x13)]
pub trait File {
    #[constructor]
    fn get_in(
        #[handle(clone)] fs: impl FileSystem,
        #[lease] path: &[u8],
    ) -> Result<Self, OpenError>;

    #[constructor]
    fn get(#[lease] path: &[u8]) -> Result<Self, OpenError>;

    #[constructor]
    fn get_or_create_in(
        #[handle(clone)] fs: impl FileSystem,
        #[lease] path: &[u8],
    ) -> Result<Self, OpenError>;

    #[constructor]
    fn get_or_create(#[lease] path: &[u8]) -> Result<Self, OpenError>;

    #[message]
    fn read(&self, offset: FileOffset, #[lease] buf: &mut [u8]) -> u32;

    #[message]
    fn write(&self, offset: FileOffset, #[lease] buf: &[u8]) -> u32;

    #[message]
    fn size(&self) -> u32;

    /// Mark the file for deletion. The actual `lfs_remove` is deferred until
    /// every open handle to this path has been closed.
    #[message]
    fn unlink(&self);

    #[destructor]
    fn close(self);
}

/// An open directory for iteration.
#[ipc::resource(arena_size = 4, kind = 0x15, clone = refcount)]
pub trait Folder {
    /// Open a directory.  `fs_id` is obtained from `FileSystem::id()`.
    #[constructor]
    fn get(#[handle(clone)] fs: impl FileSystem, #[lease] path: &[u8]) -> Result<Self, OpenError>;

    #[constructor]
    fn get_or_create(
        #[handle(clone)] fs: impl FileSystem,
        #[lease] path: &[u8],
    ) -> Result<Self, OpenError>;
}

#[ipc::resource(arena_size = 4, kind = 0x16)]
pub trait FolderIterator {
    #[constructor]
    fn iter(#[handle(clone)] folder: impl Folder) -> Self;

    #[message]
    fn next(&self) -> Option<DirEntry>;
}
