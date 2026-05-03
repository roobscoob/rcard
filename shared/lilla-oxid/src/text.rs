#[cfg(feature = "std")]
use std::io::{self, BufRead, Read};
#[cfg(feature = "std")]
use std::path::Path;

use crate::graphics::{
    Color, Image, ImageFormat, Point, Rect, ROSubImage, ReadSurface, Surface,
    draw_pixel, read_pixel,
};
#[cfg(feature = "std")]
use crate::graphics::{image_load_pbm_from, image_load_pgm_from};
#[cfg(feature = "alloc")]
use crate::graphics::{image_parse_pbm, image_parse_pgm, ParseError};
#[cfg(all(not(feature = "std"), feature = "alloc"))]
use alloc::vec::Vec;

// --- Font (lightweight rendering handle; borrows its backing data) ---
//
// `'a` is the lifetime of the glyph/kern slices and the sheet pixels.
// For flash-resident fonts, `'a` is `'static`.
// For RAM-mapped or runtime-loaded fonts, `'a` is the lifetime of the
// owning `FontData`.  Construct via `FontData::font()`.

pub struct Font<'a> {
    pub cols:     u16,
    pub rows:     u16,
    pub tile_w:   u16,
    pub tile_h:   u16,
    pub baseline: u16,
    pub line_gap: u16,
    pub space_w:  u16,
    num_glyphs:   u16,
    glyph_data:   &'a [u8],       // num_glyphs * 8 bytes; parsed on demand
    kern_data:    &'a [u8],       // raw kern section; scanned in-place
    pub sheet:    ROSubImage<'a>, // read-only view into the decoded tile sheet
}

// Transient — not stored in Font.
struct GlyphInfo { lsb: i16, adv: i16 }

impl<'a> Font<'a> {
    // Parse one 8-byte glyph record by slot index — no allocation.
    fn glyph_at(&self, i: usize) -> (char, GlyphInfo) {
        let o   = i * 8;
        let cp  = u32::from_be_bytes(self.glyph_data[o..o+4].try_into().unwrap());
        let lsb = i16::from_be_bytes(self.glyph_data[o+4..o+6].try_into().unwrap());
        let adv = i16::from_be_bytes(self.glyph_data[o+6..o+8].try_into().unwrap());
        (char::from_u32(cp).unwrap_or('\0'), GlyphInfo { lsb, adv })
    }

    // Linear scan; returns (tile_slot_index, GlyphInfo).
    fn find_glyph(&self, ch: char) -> Option<(usize, GlyphInfo)> {
        for i in 0..self.num_glyphs as usize {
            let (c, g) = self.glyph_at(i);
            if c == ch { return Some((i, g)); }
        }
        None
    }

    // In-place linear scan of kern_data — no allocation.
    //
    // Each kern record:  [4-byte total_len][2-byte i16 kern][UTF-8 string]
    // The string's first char is the left codepoint; the rest are right codepoints
    // that all share the same kern value.  We walk the string in-place without
    // reassembling any pairs.
    fn kern_between(&self, left: char, right: char) -> i32 {
        let mut pos = 0;
        while pos + 6 <= self.kern_data.len() {
            let total = u32::from_be_bytes(
                self.kern_data[pos..pos+4].try_into().unwrap()
            ) as usize;
            if total < 6 || pos + total > self.kern_data.len() { break; }

            let kern = i16::from_be_bytes(
                self.kern_data[pos+4..pos+6].try_into().unwrap()
            ) as i32;

            let s_bytes = &self.kern_data[pos+6..pos+total];
            if let Ok(s) = core::str::from_utf8(s_bytes) {
                let mut chars = s.chars();
                if chars.next() == Some(left) && chars.any(|c| c == right) {
                    return kern;
                }
            }

            pos += total;
        }
        0
    }
}

// --- FontData (owned; produces Font<'_> on demand) ---
//
// This is the normal entry point for loading fonts.  It owns the glyph/kern
// byte arrays and the decoded sheet image.  Call `.font()` to get a `Font<'_>`
// that borrows from `self` for rendering.

#[cfg(feature = "alloc")]
pub struct FontData {
    cols:       u16,
    rows:       u16,
    tile_w:     u16,
    tile_h:     u16,
    baseline:   u16,
    line_gap:   u16,
    space_w:    u16,
    num_glyphs: u16,
    glyph_data: Vec<u8>,
    kern_data:  Vec<u8>,
    sheet:      Image,
}

#[cfg(feature = "alloc")]
impl FontData {
    /// Borrow this font's data as a lightweight `Font<'_>` rendering handle.
    /// The handle is valid as long as `self` is alive.
    pub fn font(&self) -> Font<'_> {
        let sheet = self.sheet.ro_subimage(Rect {
            origin: Point { x: 0, y: 0 },
            size:   self.sheet.size,
        });
        Font {
            cols:       self.cols,
            rows:       self.rows,
            tile_w:     self.tile_w,
            tile_h:     self.tile_h,
            baseline:   self.baseline,
            line_gap:   self.line_gap,
            space_w:    self.space_w,
            num_glyphs: self.num_glyphs,
            glyph_data: &self.glyph_data,
            kern_data:  &self.kern_data,
            sheet,
        }
    }

    /// Parse a font from raw byte slices — no filesystem access required.
    /// Suitable for `include_bytes!` (flash) or runtime-loaded buffers (RAM).
    /// Copies the glyph and kern sections into owned `Vec<u8>` storage and
    /// decodes the sheet image once; the resulting `FontData` is self-contained.
    pub fn from_bytes(font_bytes: &[u8], sheet_bytes: &[u8]) -> Result<Self, ParseError> {
        if font_bytes.len() < 22 { return Err(ParseError::DataTooShort); }

        let magic = u16::from_be_bytes([font_bytes[0], font_bytes[1]]);
        if magic != 0x1BF0 { return Err(ParseError::InvalidMagic); }
        // [2] = version, [3] = reserved

        let cols       = u16::from_be_bytes([font_bytes[4],  font_bytes[5]]);
        let rows       = u16::from_be_bytes([font_bytes[6],  font_bytes[7]]);
        let tile_w     = u16::from_be_bytes([font_bytes[8],  font_bytes[9]]);
        let tile_h     = u16::from_be_bytes([font_bytes[10], font_bytes[11]]);
        let baseline   = u16::from_be_bytes([font_bytes[12], font_bytes[13]]);
        let line_gap   = u16::from_be_bytes([font_bytes[14], font_bytes[15]]);
        let space_w    = u16::from_be_bytes([font_bytes[16], font_bytes[17]]);
        let num_glyphs = u16::from_be_bytes([font_bytes[18], font_bytes[19]]);

        let glyph_end = 22 + num_glyphs as usize * 8;
        if font_bytes.len() < glyph_end { return Err(ParseError::DataTooShort); }

        let glyph_data = font_bytes[22..glyph_end].to_vec();
        let kern_data  = font_bytes[glyph_end..].to_vec();
        let sheet      = parse_sheet_from_bytes(sheet_bytes)?;

        Ok(FontData { cols, rows, tile_w, tile_h, baseline, line_gap, space_w,
                      num_glyphs, glyph_data, kern_data, sheet })
    }

    /// Load a font from any `Read` + `BufRead` pair (file, `Cursor<&[u8]>`, etc.).
    /// Reads directly into the final buffers — no intermediate full-file copy.
    #[cfg(feature = "std")]
    pub fn from_reader(mut font: impl Read, sheet: impl BufRead) -> io::Result<Self> {
        let mut hdr = [0u8; 22];
        font.read_exact(&mut hdr)?;

        let magic = u16::from_be_bytes([hdr[0], hdr[1]]);
        if magic != 0x1BF0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad font magic"));
        }

        let cols       = u16::from_be_bytes([hdr[4],  hdr[5]]);
        let rows       = u16::from_be_bytes([hdr[6],  hdr[7]]);
        let tile_w     = u16::from_be_bytes([hdr[8],  hdr[9]]);
        let tile_h     = u16::from_be_bytes([hdr[10], hdr[11]]);
        let baseline   = u16::from_be_bytes([hdr[12], hdr[13]]);
        let line_gap   = u16::from_be_bytes([hdr[14], hdr[15]]);
        let space_w    = u16::from_be_bytes([hdr[16], hdr[17]]);
        let num_glyphs = u16::from_be_bytes([hdr[18], hdr[19]]);

        let mut glyph_data = vec![0u8; num_glyphs as usize * 8];
        font.read_exact(&mut glyph_data)?;

        let mut kern_data = Vec::new();
        font.read_to_end(&mut kern_data)?;

        let sheet = load_sheet_from(sheet)?;

        Ok(FontData { cols, rows, tile_w, tile_h, baseline, line_gap, space_w,
                      num_glyphs, glyph_data, kern_data, sheet })
    }

    /// Load a font from two file paths on disk.
    #[cfg(feature = "std")]
    pub fn load(font_path: impl AsRef<Path>, sheet_path: impl AsRef<Path>) -> io::Result<Self> {
        Self::from_reader(
            std::fs::File::open(font_path)?,
            io::BufReader::new(std::fs::File::open(sheet_path)?),
        )
    }
}

// --- Sheet loading (std path) ---

#[cfg(feature = "std")]
fn load_sheet_from(mut r: impl BufRead) -> io::Result<Image> {
    let magic = {
        let buf = r.fill_buf()?;
        if buf.len() < 2 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "sheet too short"));
        }
        [buf[0], buf[1]]
    };
    match &magic {
        b"P4" => image_load_pbm_from(r),
        b"P5" => image_load_pgm_from(r, ImageFormat::EightBpp),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported sheet format")),
    }
}

// --- Sheet loading (byte-slice path, no I/O) ---

#[cfg(feature = "alloc")]
fn parse_sheet_from_bytes(data: &[u8]) -> Result<Image, ParseError> {
    if data.len() < 2 { return Err(ParseError::DataTooShort); }
    match &data[0..2] {
        b"P4" => image_parse_pbm(data),
        b"P5" => image_parse_pgm(data, ImageFormat::EightBpp),
        _     => Err(ParseError::UnsupportedFormat),
    }
}

// --- Drawing ---
//
// Convention: Color::Black in the sheet tile is treated as alpha (transparent).
// Any non-black pixel causes a draw_pixel(dest, ..., color) — same vibe as mask().

/// Draw one character at `at` using `color` for all opaque pixels.
/// Returns the x advance in pixels.
pub fn draw_char(dest: &mut impl Surface, font: &Font<'_>, ch: char, at: Point, color: Color) -> i32 {
    if ch == ' ' {
        return font.space_w as i32;
    }

    let (idx, glyph) = match font.find_glyph(ch) {
        Some(g) => g,
        None    => return 0,
    };

    let tx0 = (idx % font.cols as usize) as i32 * font.tile_w as i32;
    let ty0 = (idx / font.cols as usize) as i32 * font.tile_h as i32;

    for ty in 0..font.tile_h as i32 {
        for tx in 0..glyph.adv as i32 {
            if read_pixel(&font.sheet, Point { x: tx0 + tx, y: ty0 + ty }) != Color::Black {
                draw_pixel(dest, Point {
                    x: at.x + glyph.lsb as i32 + tx,
                    y: at.y + ty,
                }, color);
            }
        }
    }

    glyph.adv as i32
}

/// Draw a string with kerning applied between adjacent characters.
/// Returns the final cursor x position (i.e. at.x + total advance).
pub fn draw_str(dest: &mut impl Surface, font: &Font<'_>, s: &str, at: Point, color: Color) -> i32 {
    let mut x    = at.x;
    let mut prev = None::<char>;
    for ch in s.chars() {
        if let Some(p) = prev { x += font.kern_between(p, ch); }
        x += draw_char(dest, font, ch, Point { x, y: at.y }, color);
        prev = Some(ch);
    }
    x
}

/// Measure the pixel width of a string including kerning.
pub fn str_width(font: &Font<'_>, s: &str) -> i32 {
    let mut w    = 0i32;
    let mut prev = None::<char>;
    for ch in s.chars() {
        if let Some(p) = prev { w += font.kern_between(p, ch); }
        w += if ch == ' ' {
            font.space_w as i32
        } else {
            font.find_glyph(ch).map(|(_, g)| g.adv as i32).unwrap_or(0)
        };
        prev = Some(ch);
    }
    w
}

// --- Word-wrapped drawing ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align { Left, Right, Center }

// Scan `s` (no leading whitespace) greedily to find the longest prefix that fits
// in `max_w` pixels.  Returns (line_slice, remaining_slice): line_slice spans from
// the first character to the end of the last fitting word; remaining_slice starts
// at the first character of the next word.  At least one word is always consumed,
// so a single word wider than max_w still makes forward progress.
fn find_line_break<'a>(font: &Font<'_>, s: &'a str, max_w: i32) -> (&'a str, &'a str) {
    let base = s.as_ptr() as usize;
    let mut words = s.split_whitespace();

    let first = match words.next() {
        Some(w) => w,
        None    => return ("", ""),
    };

    let mut line_end  = first.as_ptr() as usize - base + first.len();
    let mut current_w = str_width(font, first);

    for word in words {
        let gap_w = font.space_w as i32 + str_width(font, word);
        if current_w + gap_w > max_w {
            let word_start = word.as_ptr() as usize - base;
            return (&s[..line_end], &s[word_start..]);
        }
        current_w += gap_w;
        line_end   = word.as_ptr() as usize - base + word.len();
    }

    (&s[..line_end], "")
}

// Measure the rendered pixel width of a line slice (words separated by space_w).
fn measure_line(font: &Font<'_>, line: &str) -> i32 {
    let mut w     = 0i32;
    let mut first = true;
    for word in line.split_whitespace() {
        if !first { w += font.space_w as i32; }
        w    += str_width(font, word);
        first = false;
    }
    w
}

// Draw words in a line slice separated by space_w, starting at `at`.
fn draw_line(dest: &mut impl Surface, font: &Font<'_>, line: &str, at: Point, color: Color) {
    let mut cx    = at.x;
    let mut first = true;
    for word in line.split_whitespace() {
        if !first { cx += font.space_w as i32; }
        cx    = draw_str(dest, font, word, Point { x: cx, y: at.y }, color);
        first = false;
    }
}

/// Draw `s` word-wrapped within a column of `max_w` pixels, with the given alignment.
/// Allocation-free: processes one line at a time via byte-offset slices into `s`.
pub fn draw_str_wrapped(
    dest:  &mut impl Surface,
    font:  &Font<'_>,
    s:     &str,
    at:    Point,
    max_w: i32,
    align: Align,
    color: Color,
) {
    let line_h        = font.tile_h as i32 + font.line_gap as i32;
    let mut y         = at.y;
    let mut remaining = s.trim_start();

    while !remaining.is_empty() {
        let (line, rest) = find_line_break(font, remaining, max_w);

        let lw = measure_line(font, line);
        let x0 = match align {
            Align::Left   => at.x,
            Align::Right  => at.x + max_w - lw,
            Align::Center => at.x + (max_w - lw) / 2,
        };

        draw_line(dest, font, line, Point { x: x0, y }, color);

        y         += line_h;
        remaining  = rest.trim_start();
    }
}
