#![no_std]

/// Pixel format on the wire. Discriminant matches lilla-oxid's
/// `lilla_oxid::graphics::ImageFormat` (1 / 2 / 8 bpp), so the compositor
/// can map across the IPC boundary without translation tables.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum ImageFormat {
    Mono = 1,
    TwoBpp = 2,
    EightBpp = 8,
}

impl ImageFormat {
    /// Bits per pixel (also the discriminant value).
    pub fn bpp(self) -> usize {
        self as u8 as usize
    }
}

/// Wire description of a framebuffer's pixel layout. `data_len()` is the
/// total byte length of the pixel buffer (after row-pitch padding).
///
/// Callers must validate dimensions before relying on `data_len`; the
/// computation is overflow-safe but is not meaningful for hostile inputs.
#[derive(
    Clone,
    Copy,
    Debug,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
pub struct FrameBufferInfo {
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
}

impl FrameBufferInfo {
    pub fn new(format: ImageFormat, width: u32, height: u32) -> Self {
        Self {
            format,
            width,
            height,
        }
    }

    /// Row pitch in bytes, rounded up.
    pub fn pitch(&self) -> usize {
        // Computed in u64 to avoid overflow on hostile inputs.
        let bpp = self.format.bpp() as u64;
        let w = self.width as u64;
        let bits = w.saturating_mul(bpp).saturating_add(7);
        let pitch = bits / 8;
        if pitch > usize::MAX as u64 {
            usize::MAX
        } else {
            pitch as usize
        }
    }

    /// Total pixel buffer size in bytes (`pitch * height`).
    pub fn data_len(&self) -> usize {
        let pitch = self.pitch() as u64;
        let h = self.height as u64;
        let total = pitch.saturating_mul(h);
        if total > usize::MAX as u64 {
            usize::MAX
        } else {
            total as usize
        }
    }
}

/// Stable identifier for a `FrameBuffer` resource. The compositor assigns
/// one at allocation time. Anyone holding the id can attach a `Layer` to
/// the framebuffer; only the original creator can call `write` on it.
///
/// Sharing across tasks is by-value (the inner `u32` is the entire identity).
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(transparent)]
pub struct FrameBufferId(pub u32);

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum FrameBufferError {
    /// Lease length didn't match `FrameBufferInfo::data_len()`.
    WrongSeedLength = 0,
    /// Compositor heap could not satisfy the allocation.
    OutOfMemory = 1,
    /// Caller doesn't own this framebuffer (write attempted by non-creator).
    NotOwner = 2,
    /// Width/height was zero or above the compositor's maximum.
    InvalidDimensions = 3,
    /// Compositor's framebuffer slot table is full.
    OutOfSlots = 4,
}

#[ipc::resource(arena_size = 32, kind = 0x06)]
pub trait FrameBuffer {
    /// Allocate a framebuffer with the given format/size and seed it from
    /// `seed_data` (whose length must equal `info.data_len()`).
    #[constructor]
    fn new(
        info: FrameBufferInfo,
        #[lease] seed_data: &[u8],
    ) -> Result<Self, FrameBufferError>;

    /// Replace the framebuffer's pixels. `seed_data` must match the
    /// already-fixed length of the framebuffer.
    #[message]
    fn write(&mut self, #[lease] seed_data: &[u8]) -> Result<(), FrameBufferError>;

    /// Stable identifier for this framebuffer. Pass to `Layer::new` (or
    /// hand to another task) to compose it onto the display.
    #[message]
    fn id(&self) -> FrameBufferId;
}

/// How a `Layer` blends its source onto the composite. Only `Replace` is
/// supported today (a straight blit, last-write-wins for overlap); other
/// modes are placeholders for when transparency / alpha-mask uploads land.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum BlendMode {
    Replace = 0,
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    postcard_schema::Schema,
)]
#[repr(u8)]
pub enum LayerError {
    /// The framebuffer id doesn't refer to a live framebuffer.
    UnknownFrameBuffer = 0,
    /// Compositor's layer slot table is full.
    OutOfSlots = 1,
    /// Caller doesn't own this layer (mutation attempted by non-creator).
    NotOwner = 2,
}

/// A placement of a `FrameBuffer` on the composite. Each task can own
/// many `Layer`s, all of which the compositor renders in z-order on every
/// `present` notification. Layers referencing a destroyed framebuffer
/// silently render nothing until pointed at a live one.
#[ipc::resource(arena_size = 32, kind = 0x07)]
pub trait Layer {
    /// Create a new visible layer at `(x, y, z)` rendering `framebuffer`
    /// with `blend`. Fails if the framebuffer id isn't live or no layer
    /// slot is free.
    #[constructor]
    fn new(
        framebuffer: FrameBufferId,
        x: i16,
        y: i16,
        z: i16,
        blend: BlendMode,
    ) -> Result<Self, LayerError>;

    /// Point the layer at a different framebuffer. Useful for double
    /// buffering — flip atomically between two framebuffers without
    /// recreating the layer.
    #[message]
    fn set_framebuffer(&mut self, framebuffer: FrameBufferId) -> Result<(), LayerError>;

    /// Move the layer to `(x, y)`.
    #[message]
    fn set_position(&mut self, x: i16, y: i16) -> Result<(), LayerError>;

    /// Change the layer's z-order. Higher z renders on top; ties break by
    /// allocation order.
    #[message]
    fn set_z(&mut self, z: i16) -> Result<(), LayerError>;

    /// Show or hide the layer without removing it.
    #[message]
    fn set_visible(&mut self, visible: bool) -> Result<(), LayerError>;
}
