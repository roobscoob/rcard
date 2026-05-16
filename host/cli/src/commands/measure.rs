use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;

use console::style;

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn short_name(name: &str) -> &str {
    name.strip_prefix("sysmodule_").unwrap_or(name)
}

fn region_kind(addr: u64) -> &'static str {
    if addr >= 0x2000_0000 && addr < 0x4000_0000 {
        "RAM"
    } else if addr >= 0x1000_0000 && addr < 0x2000_0000 {
        "FLASH"
    } else {
        "???"
    }
}

// ── Bar rendering ────────────────────────────────────────────────────

const BAR_WIDTH: usize = 40;
const BAR_FULL: &str = "█";
const BAR_EMPTY: &str = "░";

fn render_bar(used: u64, total: u64) -> String {
    if total == 0 {
        return BAR_EMPTY.repeat(BAR_WIDTH);
    }
    let filled = ((used as f64 / total as f64) * BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(BAR_WIDTH);
    format!(
        "{}{}",
        style(BAR_FULL.repeat(filled)).cyan(),
        style(BAR_EMPTY.repeat(BAR_WIDTH - filled)).dim(),
    )
}

// ── Entry point ──────────────────────────────────────────────────────

pub fn run(tfw_path: PathBuf) {
    let tfw_data = std::fs::read(&tfw_path).unwrap_or_else(|e| {
        eprintln!(
            "  {} {}",
            style("✗").red(),
            style(format!("cannot read {}: {e}", tfw_path.display())).red()
        );
        std::process::exit(1);
    });

    let tfw_name = tfw_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| tfw_path.display().to_string());

    // ── Parse places.bin for segment info ────────────────────────
    let places_data = {
        let cursor = std::io::Cursor::new(&tfw_data);
        let mut archive = zip::ZipArchive::new(cursor).unwrap_or_else(|e| {
            eprintln!(
                "  {} {}",
                style("✗").red(),
                style(format!("invalid .tfw: {e}")).red()
            );
            std::process::exit(1);
        });
        let mut buf = Vec::new();
        archive
            .by_name("places.bin")
            .unwrap_or_else(|e| {
                eprintln!(
                    "  {} {}",
                    style("✗").red(),
                    style(format!("no places.bin: {e}")).red()
                );
                std::process::exit(1);
            })
            .read_to_end(&mut buf)
            .unwrap_or_else(|e| {
                eprintln!(
                    "  {} {}",
                    style("✗").red(),
                    style(format!("read places.bin: {e}")).red()
                );
                std::process::exit(1);
            });
        buf
    };

    let image = rcard_places::PlacesImage::parse(&places_data).unwrap_or_else(|e| {
        eprintln!(
            "  {} {}",
            style("✗").red(),
            style(format!("parse places.bin: {e:?}")).red()
        );
        std::process::exit(1);
    });

    // ── Parse build metadata for allocation records ──────────────
    let build_meta: Option<tfw::build_metadata::BuildMetadata> = {
        let cursor = std::io::Cursor::new(&tfw_data);
        let mut archive = zip::ZipArchive::new(cursor).ok();
        archive.as_mut().and_then(|a| {
            let mut entry = a.by_name("build-metadata.json").ok()?;
            let mut json = String::new();
            entry.read_to_string(&mut json).ok()?;
            serde_json::from_str(&json).ok()
        })
    };

    // ── Header ───────────────────────────────────────────────────
    eprintln!();
    if let Some(ref meta) = build_meta {
        eprintln!(
            "  {} {} · {}",
            style("◉").cyan().bold(),
            style(&meta.name).bold(),
            style(&meta.board).dim(),
        );
        if let Some(ref version) = meta.version {
            eprintln!("  {} {}", style("version").dim(), version);
        }
        if let Some(ms) = meta.build_duration_ms {
            eprintln!(
                "  {} built in {:.1}s",
                style("duration").dim(),
                ms as f64 / 1000.0
            );
        }
    } else {
        eprintln!("  {} {}", style("◉").cyan().bold(), style(&tfw_name).bold());
    }
    eprintln!();

    // ── Segments by memory region ────────────────────────────────
    // Group segments by kind (RAM/FLASH)
    struct SegInfo {
        dest: u64,
        file_size: u64,
        mem_size: u64,
    }

    let mut by_region: BTreeMap<&str, Vec<SegInfo>> = BTreeMap::new();
    let mut total_file: u64 = 0;
    let mut total_mem: u64 = 0;

    for seg in image.segments() {
        let kind = region_kind(seg.dest() as u64);
        total_file += seg.file_size() as u64;
        total_mem += seg.mem_size() as u64;
        by_region.entry(kind).or_default().push(SegInfo {
            dest: seg.dest() as u64,
            file_size: seg.file_size() as u64,
            mem_size: seg.mem_size() as u64,
        });
    }

    eprintln!(
        "  {} segments  {} image  {} in memory",
        style(format!("{}", image.segment_count())).bold(),
        style(human_size(total_file)).cyan(),
        style(human_size(total_mem)).cyan(),
    );
    eprintln!(
        "  {} {:#010x}",
        style("entry").dim(),
        image.entry_point()
    );
    eprintln!();

    for (kind, segs) in &by_region {
        let region_file: u64 = segs.iter().map(|s| s.file_size).sum();
        let region_mem: u64 = segs.iter().map(|s| s.mem_size).sum();
        let low = segs.iter().map(|s| s.dest).min().unwrap_or(0);
        let high = segs
            .iter()
            .map(|s| s.dest + s.mem_size)
            .max()
            .unwrap_or(0);
        let span = high - low;

        eprintln!(
            "  {}  {} file, {} mem  {:#010x}..{:#010x}",
            style(format!("{kind:>5}")).bold(),
            style(human_size(region_file)).cyan(),
            human_size(region_mem),
            low,
            high,
        );
        for seg in segs {
            let zf = seg.mem_size.saturating_sub(seg.file_size);
            let zf_str = if zf > 0 {
                format!(" + {} zero", human_size(zf))
            } else {
                String::new()
            };
            eprintln!(
                "    {:#010x}  {}{zf_str}",
                seg.dest,
                style(human_size(seg.file_size)).dim(),
            );
        }
        if span > 0 {
            eprintln!("    {}", render_bar(region_mem, span));
        }
        eprintln!();
    }

    // ── Per-owner allocation breakdown (from build metadata) ─────
    if let Some(ref meta) = build_meta {
        if !meta.allocations.is_empty() {
            // Group by owner, sum sizes
            let mut by_owner: BTreeMap<&str, Vec<(&str, &str, u64, u64)>> = BTreeMap::new();
            for a in &meta.allocations {
                by_owner.entry(&a.owner).or_default().push((
                    &a.region,
                    &a.place,
                    a.base,
                    a.size,
                ));
            }

            let name_w = by_owner
                .keys()
                .map(|n| short_name(n).len())
                .max()
                .unwrap_or(8);

            eprintln!("  {}", style("allocations").bold());
            eprintln!();

            for (owner, regions) in &by_owner {
                let total: u64 = regions.iter().map(|(_, _, _, s)| s).sum();
                let dn = short_name(owner);

                let region_parts: Vec<String> = regions
                    .iter()
                    .map(|(region, _place, _base, size)| {
                        format!("{} {}", style(region).dim(), human_size(*size))
                    })
                    .collect();

                eprintln!(
                    "    {:<w$}  {:>10}  {}",
                    style(dn).green(),
                    style(human_size(total)).bold(),
                    region_parts.join("  "),
                    w = name_w,
                );
            }

            // Per-place totals
            let mut by_place: BTreeMap<&str, u64> = BTreeMap::new();
            for a in &meta.allocations {
                *by_place.entry(&a.place).or_default() += a.size;
            }
            if by_place.len() > 1 {
                eprintln!();
                for (place, total) in &by_place {
                    eprintln!(
                        "    {} {}",
                        style(format!("{place:>name_w$}")).dim(),
                        human_size(*total),
                    );
                }
            }
            eprintln!();
        }
    }

    // ── Image file size ──────────────────────────────────────────
    eprintln!(
        "  {} {} · {}",
        style("→").dim(),
        style(&tfw_name).bold(),
        style(human_size(tfw_data.len() as u64)).dim(),
    );
    eprintln!();
}
