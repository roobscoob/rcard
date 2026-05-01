// no_std support: in a no_std + alloc build the caller enables the "alloc"
// feature so Vec resolves to alloc::vec::Vec.  In std builds (the default)
// Vec comes from the prelude automatically.  The `vec!` macro similarly comes
// from alloc; in a no_std build the crate root should carry
// `#[macro_use] extern crate alloc;`.
#[cfg(all(not(feature = "std"), feature = "alloc"))]
extern crate alloc;
#[cfg(all(not(feature = "std"), feature = "alloc"))]
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Color {
    White,
    LightGrey,
    Grey,
    DarkGrey,
    Black,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Mono = 1,
    TwoBpp = 2,
    EightBpp = 8,
}

#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct Size {
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

// --- Surface traits ---
//
// `ReadSurface` — immutable access to pixel data.
//   Implemented by `Image`, `SubImage`, and `ROSubImage`.
//   Multiple `ROSubImage`s can be alive simultaneously from the same parent
//   because they only hold `&[u8]` (shared borrows freely alias).
//
// `Surface: ReadSurface` — adds mutable access and mutable subimage creation.
//   Implemented by `Image` and `SubImage` only.
//   `read_pixel`, saves, and the src side of blits only need `ReadSurface`.

pub trait ReadSurface {
    fn size(&self)   -> Size;
    fn format(&self) -> ImageFormat;
    fn pitch(&self)  -> usize;
    /// Offset of this surface's origin within the backing data buffer.
    fn sub(&self)    -> Point;
    fn data(&self)   -> &[u8];
    /// Return a read-only view at `area`. Multiple can coexist.
    fn ro_subimage(&self, area: Rect) -> ROSubImage<'_>;
}

pub trait Surface: ReadSurface {
    fn data_mut(&mut self) -> &mut [u8];
    /// Return a mutable view at `area`. Exclusively borrows the parent.
    fn subimage(&mut self, area: Rect) -> SubImage<'_>;
}

// --- Image (owned) ---

#[derive(Debug, Clone)]
pub struct Image {
    pub size: Size,
    pub format: ImageFormat,
    pub pitch: usize,
    pub data: Vec<u8>,
    pub sub: Point,
}

impl Image {
    pub fn new(size: Size, format: ImageFormat) -> Self {
        let bpp = format as usize;
        let pitch = (size.w as usize * bpp + 7) / 8;
        let data = vec![0u8; size.h as usize * pitch];
        Image { size, format, pitch, data, sub: Point { x: 0, y: 0 } }
    }

    /// Create an Image backed by an existing Vec<u8>.
    /// Returns None if `data` is too small to hold a `size` x `format` image.
    pub fn from_vec(size: Size, format: ImageFormat, data: Vec<u8>) -> Option<Self> {
        let bpp = format as usize;
        let pitch = (size.w as usize * bpp + 7) / 8;
        if data.len() < size.h as usize * pitch {
            return None;
        }
        Some(Image { size, format, pitch, data, sub: Point { x: 0, y: 0 } })
    }
}

impl ReadSurface for Image {
    fn size(&self)   -> Size        { self.size }
    fn format(&self) -> ImageFormat { self.format }
    fn pitch(&self)  -> usize       { self.pitch }
    fn sub(&self)    -> Point       { self.sub }
    fn data(&self)   -> &[u8]       { &self.data }
    fn ro_subimage(&self, area: Rect) -> ROSubImage<'_> {
        ROSubImage {
            size:   area.size,
            format: self.format,
            pitch:  self.pitch,
            sub:    Point { x: self.sub.x + area.origin.x, y: self.sub.y + area.origin.y },
            data:   &self.data,
        }
    }
}

impl Surface for Image {
    fn data_mut(&mut self) -> &mut [u8] { &mut self.data }
    fn subimage(&mut self, area: Rect) -> SubImage<'_> {
        SubImage {
            size:   area.size,
            format: self.format,
            pitch:  self.pitch,
            sub:    Point { x: self.sub.x + area.origin.x, y: self.sub.y + area.origin.y },
            data:   &mut self.data,
        }
    }
}

// --- SubImage (mutable borrowed view) ---
//
// Borrows the entire backing slice of its parent exclusively.
// While alive, the parent cannot be used — enforced by the borrow checker.

#[derive(Debug)]
pub struct SubImage<'a> {
    pub size:   Size,
    pub format: ImageFormat,
    pub pitch:  usize,
    pub sub:    Point,
    data: &'a mut [u8],
}

impl<'a> ReadSurface for SubImage<'a> {
    fn size(&self)   -> Size        { self.size }
    fn format(&self) -> ImageFormat { self.format }
    fn pitch(&self)  -> usize       { self.pitch }
    fn sub(&self)    -> Point       { self.sub }
    fn data(&self)   -> &[u8]       { self.data }
    fn ro_subimage(&self, area: Rect) -> ROSubImage<'_> {
        ROSubImage {
            size:   area.size,
            format: self.format,
            pitch:  self.pitch,
            sub:    Point { x: self.sub.x + area.origin.x, y: self.sub.y + area.origin.y },
            data:   self.data,
        }
    }
}

impl<'a> Surface for SubImage<'a> {
    fn data_mut(&mut self) -> &mut [u8] { self.data }
    fn subimage(&mut self, area: Rect) -> SubImage<'_> {
        SubImage {
            size:   area.size,
            format: self.format,
            pitch:  self.pitch,
            sub:    Point { x: self.sub.x + area.origin.x, y: self.sub.y + area.origin.y },
            data:   self.data,
        }
    }
}

// --- ROSubImage (read-only borrowed view) ---
//
// Borrows the backing slice immutably, so multiple ROSubImages can coexist
// from the same parent (or from each other) simultaneously.

#[derive(Debug, Clone, Copy)]
pub struct ROSubImage<'a> {
    pub size:   Size,
    pub format: ImageFormat,
    pub pitch:  usize,
    pub sub:    Point,
    data: &'a [u8],
}

impl<'a> ReadSurface for ROSubImage<'a> {
    fn size(&self)   -> Size        { self.size }
    fn format(&self) -> ImageFormat { self.format }
    fn pitch(&self)  -> usize       { self.pitch }
    fn sub(&self)    -> Point       { self.sub }
    fn data(&self)   -> &[u8]       { self.data }
    fn ro_subimage(&self, area: Rect) -> ROSubImage<'_> {
        ROSubImage {
            size:   area.size,
            format: self.format,
            pitch:  self.pitch,
            sub:    Point { x: self.sub.x + area.origin.x, y: self.sub.y + area.origin.y },
            data:   self.data,
        }
    }
}

/// -----------------
/// Color operations.

pub fn color_to_mono(point: Point, color: Color) -> bool {
    match color {
        Color::White    => false,
        Color::Black    => true,
        Color::Grey     => (point.x % 2) != (point.y % 2),
        Color::LightGrey => (point.x % 2 == 1) && (point.y % 3 == 2),
        Color::DarkGrey  => (point.x % 2 != 1) || (point.y % 3 != 2),
    }
}

pub fn color_to_2bpp(color: Color) -> u8 {
    if color < Color::LightGrey      { 0 }
    else if color < Color::DarkGrey  { 1 }
    else if color < Color::Black     { 2 }
    else                             { 3 }
}

pub fn color_from_2bpp(v: u8) -> Color {
    match v {
        0 => Color::White,
        1 => Color::LightGrey,
        2 => Color::DarkGrey,
        _ => Color::Black,
    }
}

impl Color {
    fn as_u8(self) -> u8 { self as u8 }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => Color::White,
            1 => Color::LightGrey,
            2 => Color::Grey,
            3 => Color::DarkGrey,
            _ => Color::Black,
        }
    }
}

/// -----------------------------------------------------
/// Operations that draw primitive shapes onto a surface.

pub fn read_pixel(surface: &impl ReadSurface, point: Point) -> Color {
    let bx = (surface.sub().x + point.x) as usize;
    let by = (surface.sub().y + point.y) as usize;
    match surface.format() {
        ImageFormat::Mono => {
            let byte = by * surface.pitch() + bx / 8;
            if surface.data()[byte] & (1 << (7 - (bx % 8))) != 0 { Color::Black } else { Color::White }
        }
        ImageFormat::TwoBpp | ImageFormat::EightBpp => {
            let bpp = surface.format() as usize;
            let ppb = 8 / bpp;
            let byte  = by * surface.pitch() + bx / ppb;
            let mask  = (1u8 << bpp) - 1;
            let shift = bpp * (ppb - 1 - bx % ppb);
            let found = (surface.data()[byte] & (mask << shift)) >> shift;
            match surface.format() {
                ImageFormat::TwoBpp  => color_from_2bpp(found),
                ImageFormat::EightBpp => Color::from_u8(found),
                _ => Color::White,
            }
        }
    }
}

pub fn draw_pixel(surface: &mut impl Surface, point: Point, color: Color) {
    if point.x < 0 || point.y < 0 || point.x >= surface.size().w || point.y >= surface.size().h {
        return;
    }
    let bx = (surface.sub().x + point.x) as usize;
    let by = (surface.sub().y + point.y) as usize;
    match surface.format() {
        ImageFormat::Mono => {
            let byte = by * surface.pitch() + bx / 8;
            let bit  = 1u8 << (7 - (bx % 8));
            if color_to_mono(point, color) {
                surface.data_mut()[byte] |= bit;
            } else {
                surface.data_mut()[byte] &= !bit;
            }
        }
        ImageFormat::TwoBpp | ImageFormat::EightBpp => {
            let bpp = surface.format() as usize;
            let ppb = 8 / bpp;
            let byte     = by * surface.pitch() + bx / ppb;
            let mask     = (1u8 << bpp) - 1;
            let shift    = bpp * (ppb - 1 - bx % ppb);
            let cleared  = surface.data()[byte] & !(mask << shift);
            let to_write = match surface.format() {
                ImageFormat::TwoBpp   => color_to_2bpp(color),
                ImageFormat::EightBpp => color.as_u8(),
                _ => 0,
            };
            surface.data_mut()[byte] = cleared | ((to_write & mask) << shift);
        }
    }
}

// --- Line drawing (Bresenham) ---

pub fn draw_line(surface: &mut impl Surface, mut start: Point, end: Point, color: Color, thickness: Option<u32>) {
    let dx = (end.x - start.x).abs();
    let sx = if start.x < end.x { 1 } else { -1 };
    let dy = (end.y - start.y).abs();
    let sy = if start.y < end.y { 1 } else { -1 };
    let mut err = if dx > dy { dx } else { -dy } / 2;
    let half = thickness.unwrap_or(1).max(1) as i32 / 2;

    loop {
        for oy in -half..=half {
            for ox in -half..=half {
                draw_pixel(surface, Point { x: start.x + ox, y: start.y + oy }, color);
            }
        }
        if start.x == end.x && start.y == end.y { break; }
        let e2 = err;
        if e2 > -dx { err -= dy; start.x += sx; }
        if e2 <  dy { err += dx; start.y += sy; }
    }
}

// --- Rectangle ---

pub fn draw_rect(surface: &mut impl Surface, rect: Rect, color: Color) {
    let x0 = rect.origin.x;
    let y0 = rect.origin.y;
    let x1 = x0 + rect.size.w;
    let y1 = y0 + rect.size.h;
    draw_line(surface, Point { x: x0, y: y0 }, Point { x: x1, y: y0 }, color, None);
    draw_line(surface, Point { x: x0, y: y0 }, Point { x: x0, y: y1 }, color, None);
    draw_line(surface, Point { x: x1, y: y0 }, Point { x: x1, y: y1 }, color, None);
    draw_line(surface, Point { x: x0, y: y1 }, Point { x: x1, y: y1 }, color, None);
}

fn fill_rect(surface: &mut impl Surface, rect: Rect, color: Color) {
    for yo in 0..rect.size.h {
        draw_line(
            surface,
            Point { x: rect.origin.x,               y: rect.origin.y + yo },
            Point { x: rect.origin.x + rect.size.w, y: rect.origin.y + yo },
            color,
            None,
        );
    }
}

// --- Circle (Jesko's algorithm) ---

pub fn draw_circle(surface: &mut impl Surface, center: Point, r: u16, color: Color) {
    let r = r as i32;
    let mut t1 = r / 16;
    let mut fx = r;
    let mut fy = 0i32;

    while fx >= fy {
        draw_pixel(surface, Point { x: center.x + fx, y: center.y + fy }, color);
        draw_pixel(surface, Point { x: center.x - fx, y: center.y - fy }, color);
        draw_pixel(surface, Point { x: center.x + fx, y: center.y - fy }, color);
        draw_pixel(surface, Point { x: center.x - fx, y: center.y + fy }, color);
        draw_pixel(surface, Point { x: center.x + fy, y: center.y + fx }, color);
        draw_pixel(surface, Point { x: center.x - fy, y: center.y - fx }, color);
        draw_pixel(surface, Point { x: center.x + fy, y: center.y - fx }, color);
        draw_pixel(surface, Point { x: center.x - fy, y: center.y + fx }, color);

        fy += 1;
        t1 += fy;
        let t2 = t1 - fx;
        if t2 >= 0 { t1 = t2; fx -= 1; }
    }
}

pub fn fill_circle(surface: &mut impl Surface, center: Point, r: u16, color: Color) {
    let r = r as i32;
    let mut t1 = r / 16;
    let mut fx = r;
    let mut fy = 0i32;

    while fx >= fy {
        draw_line(surface, Point { x: center.x - fx, y: center.y + fy }, Point { x: center.x + fx, y: center.y + fy }, color, None);
        draw_line(surface, Point { x: center.x - fx, y: center.y - fy }, Point { x: center.x + fx, y: center.y - fy }, color, None);
        draw_line(surface, Point { x: center.x - fy, y: center.y + fx }, Point { x: center.x + fy, y: center.y + fx }, color, None);
        draw_line(surface, Point { x: center.x - fy, y: center.y - fx }, Point { x: center.x + fy, y: center.y - fx }, color, None);

        fy += 1;
        t1 += fy;
        let t2 = t1 - fx;
        if t2 >= 0 { t1 = t2; fx -= 1; }
    }
}

// --- Rounded rectangle ---

pub fn draw_roundrect(surface: &mut impl Surface, bounds: Rect, cr: u16, color: Color) {
    let cr = cr as i32;
    let ul = Point { x: bounds.origin.x + cr,                 y: bounds.origin.y + cr };
    let ur = Point { x: bounds.origin.x + bounds.size.w - cr, y: bounds.origin.y + cr };
    let ll = Point { x: bounds.origin.x + cr,                 y: bounds.origin.y + bounds.size.h - cr };
    let lr = Point { x: bounds.origin.x + bounds.size.w - cr, y: bounds.origin.y + bounds.size.h - cr };

    let mut t1 = cr / 16;
    let mut fx = cr;
    let mut fy = 0i32;

    while fx >= fy {
        draw_pixel(surface, Point { x: lr.x + fx, y: lr.y + fy }, color);
        draw_pixel(surface, Point { x: ul.x - fx, y: ul.y - fy }, color);
        draw_pixel(surface, Point { x: ur.x + fx, y: ur.y - fy }, color);
        draw_pixel(surface, Point { x: ll.x - fx, y: ll.y + fy }, color);
        draw_pixel(surface, Point { x: lr.x + fy, y: lr.y + fx }, color);
        draw_pixel(surface, Point { x: ul.x - fy, y: ul.y - fx }, color);
        draw_pixel(surface, Point { x: ur.x + fy, y: ur.y - fx }, color);
        draw_pixel(surface, Point { x: ll.x - fy, y: ll.y + fx }, color);

        fy += 1;
        t1 += fy;
        let t2 = t1 - fx;
        if t2 >= 0 { t1 = t2; fx -= 1; }
    }

    draw_line(surface, Point { x: bounds.origin.x + cr,               y: bounds.origin.y                 }, Point { x: bounds.origin.x + bounds.size.w - cr, y: bounds.origin.y                 }, color, None);
    draw_line(surface, Point { x: bounds.origin.x + cr,               y: bounds.origin.y + bounds.size.h }, Point { x: bounds.origin.x + bounds.size.w - cr, y: bounds.origin.y + bounds.size.h }, color, None);
    draw_line(surface, Point { x: bounds.origin.x,                    y: bounds.origin.y + cr            }, Point { x: bounds.origin.x,                    y: bounds.origin.y + bounds.size.h - cr }, color, None);
    draw_line(surface, Point { x: bounds.origin.x + bounds.size.w,    y: bounds.origin.y + cr            }, Point { x: bounds.origin.x + bounds.size.w,    y: bounds.origin.y + bounds.size.h - cr }, color, None);
}

pub fn fill_roundrect(surface: &mut impl Surface, bounds: Rect, cr: u16, color: Color) {
    let cr = cr as i32;
    let ul = Point { x: bounds.origin.x + cr,                 y: bounds.origin.y + cr };
    let ur = Point { x: bounds.origin.x + bounds.size.w - cr, y: bounds.origin.y + cr };
    let ll = Point { x: bounds.origin.x + cr,                 y: bounds.origin.y + bounds.size.h - cr };
    let lr = Point { x: bounds.origin.x + bounds.size.w - cr, y: bounds.origin.y + bounds.size.h - cr };

    // Fill the four corner arcs as horizontal spans using Jesko's algorithm.
    let mut t1 = cr / 16;
    let mut fx = cr;
    let mut fy = 0i32;

    while fx >= fy {
        // Upper corners: span from ul's left to ur's right at ul.y - fy / ul.y - fx
        draw_line(surface, Point { x: ul.x - fx, y: ul.y - fy }, Point { x: ur.x + fx, y: ul.y - fy }, color, None);
        draw_line(surface, Point { x: ul.x - fy, y: ul.y - fx }, Point { x: ur.x + fy, y: ul.y - fx }, color, None);
        // Lower corners: span from ll's left to lr's right at ll.y + fy / ll.y + fx
        draw_line(surface, Point { x: ll.x - fx, y: ll.y + fy }, Point { x: lr.x + fx, y: ll.y + fy }, color, None);
        draw_line(surface, Point { x: ll.x - fy, y: ll.y + fx }, Point { x: lr.x + fy, y: ll.y + fx }, color, None);

        fy += 1;
        t1 += fy;
        let t2 = t1 - fx;
        if t2 >= 0 { t1 = t2; fx -= 1; }
    }

    // Fill the rectangular body between the corner centres.
    fill_rect(surface, Rect {
        origin: Point { x: bounds.origin.x, y: ul.y },
        size:   Size  { w: bounds.size.w,   h: ll.y - ul.y + 1 },
    }, color);
}

/// ---------------------------------------
/// Operations that modify a whole surface.

// --- Clear ---

pub fn clear(surface: &mut impl Surface, area: Option<Rect>, color: Color) {
    let size = surface.size();
    let dest = area.unwrap_or(Rect { origin: Point { x: 0, y: 0 }, size });

    // Fast path: mono, clearing full-width rows aligned to the buffer origin.
    let aligned = surface.sub().x == 0 && dest.origin.x == 0 && dest.size.w == size.w;
    if surface.format() == ImageFormat::Mono && aligned {
        let fill_byte = match color {
            Color::White => Some(0x00u8),
            Color::Black => Some(0xFFu8),
            _ => None,
        };
        if let Some(b) = fill_byte {
            let pitch = surface.pitch();
            let start = (surface.sub().y as usize + dest.origin.y as usize) * pitch;
            let len   = dest.size.h as usize * pitch;
            surface.data_mut()[start..start + len].fill(b);
            return;
        }
    }
    fill_rect(surface, dest, color);
}

// --- Invert ---

pub fn invert(surface: &mut impl Surface, area: Option<Rect>) {
    if surface.format() != ImageFormat::Mono { return; }
    let size  = surface.size();
    let area  = area.unwrap_or(Rect { origin: Point { x: 0, y: 0 }, size });
    let sub   = surface.sub();
    let pitch = surface.pitch();

    for py in area.origin.y..area.origin.y + area.size.h {
        for px in area.origin.x..area.origin.x + area.size.w {
            let bx = (sub.x + px) as usize;
            let by = (sub.y + py) as usize;
            let byte = by * pitch + bx / 8;
            surface.data_mut()[byte] ^= 1 << (7 - (bx % 8));
        }
    }
}

// --- Scroll ---
//
// Scrolls the surface upward by `delta` rows. Correct for mono surfaces where
// sub.x == 0 (i.e., rows start at a byte boundary and span the full pitch).

pub fn scroll(surface: &mut impl Surface, delta: i32, clear_color: Color) {
    if delta == 0 { return; }
    let size = surface.size();
    if delta >= size.h || delta <= -surface.size().h {
        clear(surface, None, clear_color);
        return;
    }
    // TODO: negative (upward) scroll direction
    if delta > 0 && surface.format() == ImageFormat::Mono && surface.sub().x == 0 {
        let pitch      = surface.pitch();
        let row_start  = surface.sub().y as usize * pitch;
        let total      = size.h as usize * pitch;
        let offset     = delta as usize * pitch;
        surface.data_mut().copy_within(row_start + offset..row_start + total, row_start);
    }
    clear(surface, Some(Rect {
        origin: Point { x: 0, y: size.h - delta },
        size:   Size  { w: size.w, h: delta },
    }), clear_color);
}

/// --------------------------------------------------
/// Operations that blit surfaces onto other surfaces.

// --- Image blitting ---

pub fn draw_image(dest: &mut impl Surface, src: &impl ReadSurface, pos: Point) {
    let mut iw  = src.size().w;
    let mut ixs = 0i32;

    if pos.x < 0 {
        iw += pos.x;
        if iw <= 0 { return; }
        ixs = -pos.x;
    } else if pos.x >= dest.size().w {
        return;
    } else if pos.x + src.size().w > dest.size().w {
        iw = dest.size().w - pos.x;
    }

    for iy in 0..src.size().h {
        for ix in ixs..ixs + iw {
            let tgt = read_pixel(src, Point { x: ix, y: iy });
            draw_pixel(dest, Point { x: pos.x + ix, y: pos.y + iy }, tgt);
        }
    }
}

/// Blit `src` onto `dest` at `pos`, drawing only pixels where the corresponding
/// `mask` pixel is `Color::Black`.  Where the mask is any other color the
/// destination is left untouched, giving a fully transparent background blit.
///
/// `mask` is expected to be the same dimensions as `src`; if they differ only
/// the overlapping region (min of both sizes) is considered.  Out-of-bounds
/// destination pixels are silently clipped by `draw_pixel`.
pub fn draw_image_masked(
    dest: &mut impl Surface,
    src:  &impl ReadSurface,
    mask: &impl ReadSurface,
    pos:  Point,
) {
    let w = src.size().w.min(mask.size().w);
    let h = src.size().h.min(mask.size().h);
    for iy in 0..h {
        for ix in 0..w {
            if read_pixel(mask, Point { x: ix, y: iy }) == Color::Black {
                let p = read_pixel(src, Point { x: ix, y: iy });
                draw_pixel(dest, Point { x: pos.x + ix, y: pos.y + iy }, p);
            }
        }
    }
}

pub fn draw_image_mirrored(dest: &mut impl Surface, src: &impl ReadSurface, pos: Point) {
    for iy in 0..src.size().h {
        for ix in 0..src.size().w {
            let tgt = read_pixel(src, Point { x: src.size().w - 1 - ix, y: src.size().h - 1 - iy });
            draw_pixel(dest, Point { x: pos.x + ix, y: pos.y + iy }, tgt);
        }
    }
}

pub fn draw_image_clipped(dest: &mut impl Surface, src: &impl ReadSurface, src_rect: Option<Rect>, dest_rect: Option<Rect>) {
    let c_src  = src_rect.unwrap_or(Rect { origin: Point { x: 0, y: 0 }, size: src.size() });
    let mut c_dest = dest_rect.unwrap_or(Rect { origin: Point { x: 0, y: 0 }, size: c_src.size });
    if c_dest.size.w == 0 || c_dest.size.h == 0 { c_dest.size = c_src.size; }

    let mut sy = 0u32;
    for dy in c_dest.origin.y..=c_dest.origin.y + c_dest.size.h {
        let mut sx = 0u32;
        for dx in c_dest.origin.x..=c_dest.origin.x + c_dest.size.w {
            let picked = read_pixel(src, Point {
                x: sx as i32 + c_src.origin.x,
                y: sy as i32 + c_src.origin.y,
            });
            draw_pixel(dest, Point { x: dx, y: dy }, picked);
            sx = (sx + 1) % c_src.size.w as u32;
        }
        sy = (sy + 1) % c_src.size.h as u32;
    }
}

// --- Mask generation ---
//
// Produces a mono Image where a pixel is set (black) if any source pixel
// within `grow` distance is not the `alpha` color.  With grow=0 this is a
// straight 1:1 transcription; with grow>0 the mask is dilated by that many
// pixels in every direction, which is useful for generating borders/outlines.

/// If `dest` is provided it is used as-is; otherwise a Mono Image sized to
/// `src` is allocated.  The output is cleared to White before writing.
pub fn mask(src: &impl ReadSurface, grow: Option<u32>, alpha: Option<Color>, dest: Option<Image>) -> Image {
    let grow  = grow.unwrap_or(0) as i32;
    let alpha = alpha.unwrap_or(Color::Black);
    let size  = src.size();
    let mut out = dest.unwrap_or_else(|| Image::new(size, ImageFormat::Mono));
    clear(&mut out, None, Color::White);

    for y in 0..size.h {
        for x in 0..size.w {
            let set = if grow == 0 {
                read_pixel(src, Point { x, y }) != alpha
            } else {
                'search: {
                    for dy in -grow..=grow {
                        for dx in -grow..=grow {
                            let nx = x + dx;
                            let ny = y + dy;
                            if nx >= 0 && ny >= 0 && nx < size.w && ny < size.h
                                && read_pixel(src, Point { x: nx, y: ny }) != alpha
                            {
                                break 'search true;
                            }
                        }
                    }
                    false
                }
            };
            if set {
                draw_pixel(&mut out, Point { x, y }, Color::Black);
            }
        }
    }
    out
}

// --- Parse error (available without std) ---

/// Error returned by the byte-slice image and font parsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    InvalidMagic,
    InvalidHeader,
    DataTooShort,
    UnsupportedFormat,
}

/// Parse one unsigned decimal integer from a PNM-style header byte slice.
/// Skips leading ASCII whitespace and `#…\n` comments; advances `*pos` past
/// the token and its one mandatory trailing whitespace delimiter.
fn parse_pnm_uint(data: &[u8], pos: &mut usize) -> Option<u32> {
    loop {
        if *pos >= data.len() { return None; }
        match data[*pos] {
            b'#' => { while *pos < data.len() && data[*pos] != b'\n' { *pos += 1; } }
            b if b.is_ascii_whitespace() => { *pos += 1; }
            _ => break,
        }
    }
    if !data[*pos].is_ascii_digit() { return None; }
    let mut val: u32 = 0;
    while *pos < data.len() && data[*pos].is_ascii_digit() {
        val = val.wrapping_mul(10).wrapping_add((data[*pos] - b'0') as u32);
        *pos += 1;
    }
    // consume one mandatory trailing whitespace delimiter
    if *pos < data.len() && data[*pos].is_ascii_whitespace() { *pos += 1; }
    Some(val)
}

/// Parse a binary PBM (P4) image from a raw byte slice — no I/O, no `std`.
/// Suitable for `include_bytes!` (flash-resident sheets) or RAM-mapped buffers.
#[cfg(feature = "alloc")]
pub fn image_parse_pbm(data: &[u8]) -> Result<Image, ParseError> {
    if data.len() < 2 || &data[0..2] != b"P4" {
        return Err(ParseError::InvalidMagic);
    }
    let mut pos = 2;
    let w = parse_pnm_uint(data, &mut pos).ok_or(ParseError::InvalidHeader)? as i32;
    let h = parse_pnm_uint(data, &mut pos).ok_or(ParseError::InvalidHeader)? as i32;
    let pitch  = (w as usize + 7) / 8;
    let needed = pitch * h as usize;
    if data.len().saturating_sub(pos) < needed { return Err(ParseError::DataTooShort); }
    // P4 row pitch matches our mono Image layout — direct copy is safe for
    // root images (sub.x == 0, which Image::new guarantees).
    let mut image = Image::new(Size { w, h }, ImageFormat::Mono);
    image.data.copy_from_slice(&data[pos..pos + needed]);
    Ok(image)
}

/// Parse a binary PGM (P5) image from a raw byte slice — no I/O, no `std`.
#[cfg(feature = "alloc")]
pub fn image_parse_pgm(data: &[u8], format: ImageFormat) -> Result<Image, ParseError> {
    if data.len() < 2 || &data[0..2] != b"P5" {
        return Err(ParseError::InvalidMagic);
    }
    let mut pos = 2;
    let w      = parse_pnm_uint(data, &mut pos).ok_or(ParseError::InvalidHeader)? as i32;
    let h      = parse_pnm_uint(data, &mut pos).ok_or(ParseError::InvalidHeader)? as i32;
    let maxval = parse_pnm_uint(data, &mut pos).ok_or(ParseError::InvalidHeader)?;
    if maxval == 0 || maxval > 255 { return Err(ParseError::UnsupportedFormat); }
    let needed = w as usize * h as usize;
    if data.len().saturating_sub(pos) < needed { return Err(ParseError::DataTooShort); }
    let mut image = Image::new(Size { w, h }, format);
    for i in 0..needed {
        let v = if maxval == 255 {
            data[pos + i] as u32
        } else {
            data[pos + i] as u32 * 255 / maxval
        };
        let color = match v {
            0..=50    => Color::White,
            51..=101  => Color::LightGrey,
            102..=153 => Color::Grey,
            154..=204 => Color::DarkGrey,
            _         => Color::Black,
        };
        draw_pixel(&mut image, Point { x: i as i32 % w, y: i as i32 / w }, color);
    }
    Ok(image)
}

// --- PBM / PGM file I/O ---
//
// PBM (P4): packed 1-bit pixels, MSB-first — matches our mono Image layout
//   directly, so loads/saves are a straight data copy for root images.
// PGM (P5): one byte per pixel, 0-255 grayscale.
//   Load maps the byte range to our Color enum then calls draw_pixel,
//   which handles dithering for lower bit depths.
//   Save maps Color back to a grayscale byte.
//
// Both functions work on any impl Surface (including SubImages).
// The C save_pgm incorrectly wrote P4 headers; that's fixed here.

#[cfg(feature = "std")]
use std::io::{self, BufRead, Write};
#[cfg(feature = "std")]
use std::path::Path;

/// Read whitespace-separated ASCII tokens from a netpbm header,
/// skipping `#` comment lines.
#[cfg(feature = "std")]
fn read_header_token(reader: &mut impl BufRead) -> io::Result<String> {
    loop {
        let mut token = String::new();
        // Skip leading whitespace
        loop {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            match buf[0] {
                b'#' => {
                    // Consume the rest of the comment line
                    let mut line = String::new();
                    reader.read_line(&mut line)?;
                }
                b if (b as char).is_ascii_whitespace() => continue,
                b => { token.push(b as char); break; }
            }
        }
        // Consume the rest of the token
        loop {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            if (buf[0] as char).is_ascii_whitespace() { break; }
            token.push(buf[0] as char);
        }
        if !token.is_empty() { return Ok(token); }
    }
}

#[cfg(feature = "std")]
fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

// --- PBM ---

/// Parse a binary PBM (P4) stream as a mono Image.
/// The reader must be positioned at the start of the file (magic included).
/// Use this with `io::Cursor<&[u8]>` for in-RAM/flash data or with
/// `io::BufReader<File>` for on-disk data.
#[cfg(feature = "std")]
pub fn image_load_pbm_from(mut r: impl BufRead) -> io::Result<Image> {
    if read_header_token(&mut r)? != "P4" {
        return Err(invalid("not a P4 PBM file"));
    }
    let w: i32 = read_header_token(&mut r)?.parse().map_err(|_| invalid("bad width"))?;
    let h: i32 = read_header_token(&mut r)?.parse().map_err(|_| invalid("bad height"))?;

    let mut image = Image::new(Size { w, h }, ImageFormat::Mono);
    // P4 row pitch == our mono pitch (ceil(w/8)), data is directly compatible.
    r.read_exact(&mut image.data)?;
    Ok(image)
}

/// Load a binary PBM (P4) file from disk as a mono Image.
#[cfg(feature = "std")]
pub fn image_load_pbm(path: impl AsRef<Path>) -> io::Result<Image> {
    image_load_pbm_from(io::BufReader::new(std::fs::File::open(path)?))
}

/// Save a surface as a binary PBM (P4) file.
/// Works correctly for subimages: pixels are read individually and repacked.
#[cfg(feature = "std")]
pub fn image_save_pbm(path: impl AsRef<Path>, surface: &impl ReadSurface) -> io::Result<()> {
    let mut file = io::BufWriter::new(std::fs::File::create(path)?);
    let Size { w, h } = surface.size();

    write!(file, "P4\n{} {}\n", w, h)?;

    let out_pitch = (w as usize + 7) / 8;
    let mut row = vec![0u8; out_pitch];
    for y in 0..h {
        row.iter_mut().for_each(|b| *b = 0);
        for x in 0..w {
            if read_pixel(surface, Point { x, y }) == Color::Black {
                row[x as usize / 8] |= 1 << (7 - (x % 8));
            }
        }
        file.write_all(&row)?;
    }
    Ok(())
}

// --- PGM ---

/// Parse a binary PGM (P5) stream into an Image of the given format.
/// Reads and converts one pixel at a time through the (already-buffered) reader,
/// so no intermediate pixel buffer is allocated.
/// Use with `io::Cursor<&[u8]>` for in-RAM/flash data or `io::BufReader<File>`
/// for on-disk data.
#[cfg(feature = "std")]
pub fn image_load_pgm_from(mut r: impl BufRead, format: ImageFormat) -> io::Result<Image> {
    if read_header_token(&mut r)? != "P5" {
        return Err(invalid("not a P5 PGM file"));
    }
    let w:      i32 = read_header_token(&mut r)?.parse().map_err(|_| invalid("bad width"))?;
    let h:      i32 = read_header_token(&mut r)?.parse().map_err(|_| invalid("bad height"))?;
    let maxval: u32 = read_header_token(&mut r)?.parse().map_err(|_| invalid("bad maxval"))?;
    if maxval == 0 || maxval > 255 {
        return Err(invalid("unsupported maxval (must be 1–255)"));
    }

    let mut image = Image::new(Size { w, h }, format);
    let mut buf   = [0u8; 1];
    for i in 0..(w * h) as usize {
        r.read_exact(&mut buf)?;
        // Scale to 0-255, then bucket into our five colors.
        let v = if maxval == 255 { buf[0] as u32 } else { buf[0] as u32 * 255 / maxval };
        let color = match v {
            0..=50    => Color::White,
            51..=101  => Color::LightGrey,
            102..=153 => Color::Grey,
            154..=204 => Color::DarkGrey,
            _         => Color::Black,
        };
        draw_pixel(&mut image, Point { x: i as i32 % w, y: i as i32 / w }, color);
    }
    Ok(image)
}

/// Load a binary PGM (P5) file from disk into an Image of the given format.
#[cfg(feature = "std")]
pub fn image_load_pgm(path: impl AsRef<Path>, format: ImageFormat) -> io::Result<Image> {
    image_load_pgm_from(io::BufReader::new(std::fs::File::open(path)?), format)
}

/// Save a surface as a binary PGM (P5) file.
/// Color values are mapped to grayscale bytes (White=0 … Black=255).
#[cfg(feature = "std")]
pub fn image_save_pgm(path: impl AsRef<Path>, surface: &impl ReadSurface) -> io::Result<()> {
    let mut file = io::BufWriter::new(std::fs::File::create(path)?);
    let Size { w, h } = surface.size();

    write!(file, "P5\n{} {}\n255\n", w, h)?;

    for y in 0..h {
        for x in 0..w {
            let byte: u8 = match read_pixel(surface, Point { x, y }) {
                Color::White    =>   0,
                Color::LightGrey =>  85,
                Color::Grey     => 128,
                Color::DarkGrey => 170,
                Color::Black    => 255,
            };
            file.write_all(&[byte])?;
        }
    }
    Ok(())
}
