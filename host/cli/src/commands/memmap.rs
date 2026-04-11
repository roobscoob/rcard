use std::collections::BTreeMap;
use tfw::build::MemSegment;

// ── Colors ─────────────────────────────────────────────────────────────

type Rgb = (u8, u8, u8);

const DARK: Rgb = (18, 24, 32);
const UNUSED_COLOR: Rgb = (50, 58, 68);
const LOST_COLOR: Rgb = (220, 50, 60);

const PALETTE: &[Rgb] = &[
    (232, 201, 122), (122, 184, 232), (168, 232, 122), (232, 122, 154),
    (184, 122, 232), (122, 232, 208), (232, 160, 122), (122, 154, 232),
    (232, 224, 122), (232, 122, 232), (180, 232, 140), (232, 180, 122),
    (140, 180, 232), (232, 140, 180),
];

fn assign_colors(segments: &[MemSegment]) -> BTreeMap<String, Rgb> {
    let mut map = BTreeMap::new();
    let mut idx = 0;
    for seg in segments {
        if !map.contains_key(&seg.owner) {
            if seg.owner == "(unused)" {
                map.insert(seg.owner.clone(), UNUSED_COLOR);
            } else {
                map.insert(seg.owner.clone(), PALETTE[idx % PALETTE.len()]);
                idx += 1;
            }
        }
    }
    map
}

// ── ANSI helpers ───────────────────────────────────────────────────────

fn fg(c: Rgb, s: &str) -> String {
    format!("\x1b[38;2;{};{};{}m{s}\x1b[0m", c.0, c.1, c.2)
}

fn fgbg(fg_c: Rgb, bg_c: Rgb, s: &str) -> String {
    format!(
        "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m{s}\x1b[0m",
        fg_c.0, fg_c.1, fg_c.2, bg_c.0, bg_c.1, bg_c.2,
    )
}

fn dim(s: &str) -> String { format!("\x1b[2m{s}\x1b[0m") }
fn bold(s: &str) -> String { format!("\x1b[1m{s}\x1b[0m") }

// ── Formatting ─────────────────────────────────────────────────────────

fn fmt_size(n: u64) -> String {
    if n >= 1024 { format!("{:.1} KiB", n as f64 / 1024.0) }
    else { format!("{n} B") }
}

fn fmt_bpc(n: u64) -> String {
    if n >= 1024 { format!("~{:.0} KiB/char", n as f64 / 1024.0) }
    else { format!("~{n} B/char") }
}

fn short(name: &str) -> &str {
    name.strip_prefix("sysmodule_").unwrap_or(name)
}

// ── Layout computation ─────────────────────────────────────────────────

struct LayoutEntry {
    owner: String,
    color: Rgb,
    left: usize,   // char column start
    right: usize,  // char column end (exclusive)
    lost: u64,
    size: u64,
    base: u64,
}

fn compute_layout(
    segs: &[(String, u64, u64, u64)], // (owner, base, size, lost)
    colors: &BTreeMap<String, Rgb>,
    width: usize,
) -> Vec<LayoutEntry> {
    let n = segs.len();
    if n == 0 { return Vec::new(); }

    let min_ch = 1usize;
    let remainder = width.saturating_sub(min_ch * n);
    let sizes: Vec<u64> = segs.iter().map(|(_, _, sz, _)| *sz).collect();
    let total: u64 = sizes.iter().sum();

    let extra: Vec<usize> = if total == 0 {
        let base = remainder / n;
        let mut e = vec![base; n];
        e[n - 1] += remainder - base * n;
        e
    } else {
        sizes.iter()
            .map(|sz| (*sz as f64 / total as f64 * remainder as f64).round() as usize)
            .collect()
    };

    // Fix rounding drift.
    let actual_total: usize = extra.iter().sum::<usize>() + min_ch * n;
    let mut extra = extra;
    if actual_total != width {
        let drift = actual_total as isize - width as isize;
        if let Some(idx) = extra.iter().enumerate().max_by_key(|(_, v)| *v).map(|(i, _)| i) {
            extra[idx] = (extra[idx] as isize - drift) as usize;
        }
    }

    let mut result = Vec::with_capacity(n);
    let mut cursor = 0;
    for (i, (owner, base, size, lost)) in segs.iter().enumerate() {
        let c = colors.get(owner.as_str()).copied().unwrap_or((140, 140, 140));
        let w = min_ch + extra[i];
        result.push(LayoutEntry {
            owner: owner.clone(),
            color: c,
            left: cursor,
            right: cursor + w,
            lost: *lost,
            size: *size,
            base: *base,
        });
        cursor += w;
    }
    result
}

// ── Bar rendering ──────────────────────────────────────────────────────

const HALF: &str = "\u{258c}"; // LEFT HALF BLOCK

fn render_bar(layout: &[LayoutEntry], width: usize) -> (String, Vec<String>) {
    // Build pixel array (2 pixels per char).
    let mut pixels: Vec<(Rgb, bool)> = vec![(DARK, false); width * 2];
    for entry in layout {
        for px in (entry.left * 2)..(entry.right * 2) {
            if px < pixels.len() {
                pixels[px] = (entry.color, false);
            }
        }
        if entry.lost > 0 && entry.right > 0 {
            let idx = entry.right * 2 - 1;
            if idx < pixels.len() {
                pixels[idx] = (entry.color, true);
            }
        }
    }

    // Render bar row.
    let mut bar = String::with_capacity(width * 30);
    for ch in 0..width {
        let (lc, l_lost) = pixels[ch * 2];
        let (rc, r_lost) = pixels[ch * 2 + 1];
        if l_lost {
            bar.push_str(&fgbg(LOST_COLOR, rc, HALF));
        } else if r_lost {
            bar.push_str(&fgbg(lc, LOST_COLOR, HALF));
        } else {
            bar.push_str(&fgbg(lc, rc, HALF));
        }
    }

    // Build label rows.
    let label_rows = build_label_rows(layout, width);
    (bar, label_rows)
}

fn build_label_rows(layout: &[LayoutEntry], width: usize) -> Vec<String> {
    struct Placement {
        row: usize,
        col: usize,
        name: String,
        size_str: String,
        color: Rgb,
    }

    let mut occupied: BTreeMap<usize, Vec<(usize, usize)>> = BTreeMap::new();

    let overlaps = |occ: &BTreeMap<usize, Vec<(usize, usize)>>, row: usize, start: usize, end: usize| -> bool {
        occ.get(&row).map_or(false, |spans| {
            spans.iter().any(|(s, e)| start < *e && end > *s)
        })
    };

    let mut placements: Vec<Placement> = Vec::new();

    // Process from right to left (largest base first) like the Python.
    let mut by_base: Vec<usize> = (0..layout.len()).collect();
    by_base.sort_by(|a, b| layout[*b].base.cmp(&layout[*a].base));

    for &idx in &by_base {
        let entry = &layout[idx];
        let name = format!(" {}", short(&entry.owner));
        let size_str = format!(" {}", fmt_size(entry.size));
        let text_w = name.len().max(size_str.len());
        let padded_name = format!("{:<width$}", name, width = text_w);
        let padded_size = format!("{:<width$}", size_str, width = text_w);
        let col = entry.left.saturating_sub(1);

        for row in 0..40 {
            if !overlaps(&occupied, row, col, col + text_w)
                && !overlaps(&occupied, row + 1, col, col + text_w)
            {
                occupied.entry(row).or_default().push((col, col + text_w));
                occupied.entry(row + 1).or_default().push((col, col + text_w));
                placements.push(Placement {
                    row,
                    col,
                    name: padded_name,
                    size_str: padded_size,
                    color: entry.color,
                });
                break;
            }
        }
    }

    if placements.is_empty() {
        return Vec::new();
    }

    let max_row = placements.iter().map(|p| p.row + 1).max().unwrap_or(0);
    let w2 = width + 60;

    let mut rows: Vec<Vec<char>> = vec![vec![' '; w2]; max_row + 1];
    let mut colors: Vec<Vec<Option<Rgb>>> = vec![vec![None; w2]; max_row + 1];

    // Draw leader lines.
    for p in &placements {
        for r in 0..p.row {
            if p.col < w2 {
                rows[r][p.col] = '\u{2502}'; // │
                colors[r][p.col] = Some(p.color);
            }
        }
    }

    // Draw labels.
    for p in &placements {
        for (i, ch) in p.name.chars().enumerate() {
            if p.col + i < w2 {
                rows[p.row][p.col + i] = ch;
                colors[p.row][p.col + i] = Some(p.color);
            }
        }
        for (i, ch) in p.size_str.chars().enumerate() {
            if p.col + i < w2 {
                rows[p.row + 1][p.col + i] = ch;
                colors[p.row + 1][p.col + i] = Some(p.color);
            }
        }
    }

    // Render to strings.
    let mut result = Vec::with_capacity(max_row + 1);
    for row_idx in 0..=max_row {
        let mut line = String::new();
        for i in 0..w2 {
            let ch = rows[row_idx][i];
            if let Some(c) = colors[row_idx][i] {
                line.push_str(&fg(c, &ch.to_string()));
            } else if i < width {
                line.push(ch);
            }
        }
        // Trim trailing whitespace (but keep colored content).
        let trimmed = line.trim_end().to_string();
        result.push(trimmed);
    }
    result
}

// ── Address line ───────────────────────────────────────────────────────

fn addr_line(start: u64, end: u64, width: usize) -> String {
    let s = format!("{start:#x}");
    let e = format!("{end:#x}");
    let inner = width.saturating_sub(s.len() + e.len() + 2);
    let dots = "\u{00b7}".repeat(inner); // ·
    format!("{} {} {}", dim(&s), dim(&dots), dim(&e))
}

// ── Top-level render ───────────────────────────────────────────────────

fn rightmost_overflow(
    segs: &[(String, u64, u64, u64)],
    colors: &BTreeMap<String, Rgb>,
    w: usize,
) -> usize {
    let layout = compute_layout(segs, colors, w);
    if let Some(last) = layout.last() {
        let name = format!(" {}", short(&last.owner));
        let sz = format!(" {}", fmt_size(last.size));
        let text_w = name.len().max(sz.len());
        let col = last.left.saturating_sub(1);
        (col + text_w).saturating_sub(w)
    } else {
        0
    }
}

pub fn render(segments: &[MemSegment]) {
    if segments.is_empty() { return; }

    let colors = assign_colors(segments);

    // Group by memory region, preserving order of first appearance.
    let mut region_order: Vec<String> = Vec::new();
    let mut by_region: BTreeMap<String, Vec<(String, u64, u64, u64)>> = BTreeMap::new();
    for seg in segments {
        if !by_region.contains_key(&seg.memory) {
            region_order.push(seg.memory.clone());
        }
        by_region.entry(seg.memory.clone())
            .or_default()
            .push((seg.owner.clone(), seg.base, seg.size, seg.lost));
    }

    let term_w = terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80);
    let margin = 2;

    // Iteratively solve width so labels don't overflow.
    let mut w = term_w.saturating_sub(margin + 10);
    for _ in 0..20 {
        let overflow = by_region.values()
            .map(|segs| rightmost_overflow(segs, &colors, w))
            .max()
            .unwrap_or(0);
        let new_w = term_w.saturating_sub(margin + overflow);
        if new_w == w { break; }
        w = new_w;
    }
    w = w.max(20); // floor

    eprintln!();

    let bar_h = 3;
    for region in &region_order {
        let segs = &by_region[region];
        let total_size: u64 = segs.iter().map(|(_, _, sz, _)| *sz).sum();
        let region_start = segs.first().map(|(_, b, _, _)| *b).unwrap_or(0);
        let region_end = segs.last().map(|(_, b, sz, _)| b + sz).unwrap_or(0);

        eprintln!(
            "  {}  {}  {}",
            bold(&region.to_uppercase()),
            dim(&fmt_size(total_size)),
            dim(&fmt_bpc(total_size / w as u64)),
        );
        eprintln!("  {}", addr_line(region_start, region_end, w));

        let layout = compute_layout(segs, &colors, w);
        let (bar_row, label_rows) = render_bar(&layout, w);
        for _ in 0..bar_h {
            eprintln!("  {bar_row}");
        }
        for row in &label_rows {
            eprintln!("  {row}");
        }
        eprintln!();
    }

    // Lost bytes summary.
    let separator_w = w.min(50);
    eprintln!("  {}", dim(&"\u{2500}".repeat(separator_w))); // ─
    let lost_parts: Vec<String> = region_order.iter().map(|region| {
        let total_lost: u64 = by_region[region].iter().map(|(_, _, _, l)| *l).sum();
        format!("{} {total_lost} B", region.to_uppercase())
    }).collect();
    eprintln!(
        "  {} {}",
        fg(LOST_COLOR, "\u{2588}"), // █
        dim(&format!("lost bytes  {}", lost_parts.join("  "))),
    );
    eprintln!();
}
