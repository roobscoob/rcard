use rcard_log::OptionExt;

#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(transparent)]
pub struct BitDepth(u8);

impl BitDepth {
    pub fn new(n: u8) -> Option<Self> {
        (1..=8).contains(&n).then_some(Self(n))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

/// Channel encoding (zerocopy-safe).
///
/// Wire format (single `u8`):
/// - `0` — fixed high (white / opaque)
/// - `1` — fixed low  (black / transparent)
/// - `2..=9` — variable bit depth (value minus 1, i.e. 1–8 bits)
#[derive(
    Clone,
    Copy,
    Debug,
    zerocopy::TryFromBytes,
    zerocopy::IntoBytes,
    zerocopy::KnownLayout,
    zerocopy::Immutable,
)]
#[repr(transparent)]
pub struct Channel(u8);

impl Channel {
    pub const FIXED_HIGH: Self = Self(0);
    pub const FIXED_LOW: Self = Self(1);

    pub fn bit_depth(bits: BitDepth) -> Self {
        Self(bits.get() + 1)
    }

    pub fn fixed_value(self) -> Option<u8> {
        match self.0 {
            0 => Some(u8::MAX),
            1 => Some(0),
            _ => None,
        }
    }

    pub fn bits(self) -> usize {
        match self.0 {
            0 | 1 => 0,
            n => (n - 1) as usize,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Pixel {
    pub color: u8,
    pub alpha: u8,
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
#[repr(C, packed)]
pub struct ImageFormat {
    pub color_channel: Channel,
    pub alpha_channel: Channel,
    pub width: u32,
    pub height: u32,
}

impl ImageFormat {
    pub fn new(color_channel: Channel, alpha_channel: Channel, width: u32, height: u32) -> Self {
        Self {
            color_channel,
            alpha_channel,
            width,
            height,
        }
    }

    pub fn color_bits(&self) -> usize {
        self.color_channel.bits()
    }

    pub fn alpha_bits(&self) -> usize {
        self.alpha_channel.bits()
    }

    pub fn storage_bits_per_pixel(&self) -> usize {
        let bpp = self.color_bits() + self.alpha_bits();
        if bpp == 0 {
            0
        } else {
            bpp.next_power_of_two()
        }
    }

    pub fn storage_size_bytes(&self) -> usize {
        let bpp = self.storage_bits_per_pixel();
        (self.width as usize * self.height as usize * bpp).div_ceil(8)
    }

    pub fn pixels<I: Iterator<Item = u8>>(self, bytes: I) -> PixelIter<I> {
        let color_bits = self.color_bits();
        let alpha_bits = self.alpha_bits();
        let storage_bpp = self.storage_bits_per_pixel();

        let fixed_color = self.color_channel.fixed_value();
        let fixed_alpha = self.alpha_channel.fixed_value();

        PixelIter {
            bytes,
            fixed_color,
            fixed_alpha,
            color_mask: (1u16 << color_bits) - 1,
            alpha_mask: (1u16 << alpha_bits) - 1,
            alpha_shift: color_bits as u8,
            storage_bpp: storage_bpp as u8,
            pixels_remaining: self.width * self.height,
            current: 0,
            bits_left: 0,
        }
    }
}

pub struct PixelIter<I> {
    bytes: I,
    fixed_color: Option<u8>,
    fixed_alpha: Option<u8>,
    color_mask: u16,
    alpha_mask: u16,
    alpha_shift: u8,
    storage_bpp: u8,
    pixels_remaining: u32,
    current: u16,
    bits_left: u8,
}

impl<I: Iterator<Item = u8>> Iterator for PixelIter<I> {
    type Item = Pixel;

    fn next(&mut self) -> Option<Pixel> {
        if self.pixels_remaining == 0 {
            return None;
        }
        self.pixels_remaining -= 1;

        // Both channels fixed — no bytes consumed.
        if self.storage_bpp == 0 {
            return Some(Pixel {
                color: self
                    .fixed_color
                    .log_expect("Storage bpp 0, with no fixed color"),
                alpha: self
                    .fixed_alpha
                    .log_expect("Storage bpp 0, with no fixed alpha"),
            });
        }

        // Fill until we have enough bits for one pixel.
        while self.bits_left < self.storage_bpp {
            let byte = self.bytes.next().unwrap_or(0);
            self.current |= (byte as u16) << self.bits_left;
            self.bits_left += 8;
        }

        let raw = self.current;

        if self.storage_bpp < 16 {
            self.current >>= self.storage_bpp;
        } else {
            self.current = 0;
        }

        self.bits_left -= self.storage_bpp;

        let color = self.fixed_color.unwrap_or((raw & self.color_mask) as u8);
        let alpha = self
            .fixed_alpha
            .unwrap_or(((raw >> self.alpha_shift) & self.alpha_mask) as u8);

        Some(Pixel { color, alpha })
    }
}
