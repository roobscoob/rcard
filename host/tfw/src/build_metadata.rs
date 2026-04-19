use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// A single cargo JSON message scoped to the crate that produced it.
/// Persisted in the archive so the GUI can render build diagnostics
/// and compiler output for loaded firmware.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CargoMessageRecord {
    pub crate_name: String,
    /// One line of raw cargo JSON (ndjson). Can be decoded with
    /// `escargot::format::Message` or `serde_json`.
    pub raw: String,
}

/// One resolved region allocation — `owner.region` occupies `size`
/// bytes at cpu `base`, landing in `place`. Persisted in the archive
/// so tooling (and the GUI's memory-map card) can show real utilisation
/// without re-running the build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationRecord {
    pub place: String,
    pub owner: String,
    pub region: String,
    pub base: u64,
    pub size: u64,
    pub requested_place: String,
}

/// Metadata about a firmware build, stored in the .tfw archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildMetadata {
    /// Random UUID for this build.
    pub build_id: String,
    /// ISO 8601 timestamp of when the build completed.
    pub built_at: String,
    /// App version from the Nickel config (e.g. "0.1.0").
    pub version: Option<String>,
    /// App name from the Nickel config (e.g. "rcard").
    pub name: String,
    /// Nickel config filename stem (e.g. "fob" for `apps/fob.ncl`).
    #[serde(default)]
    pub config: String,
    /// Board filename stem (e.g. "bentoboard" for `boards/bentoboard.ncl`).
    pub board: String,
    /// Layout filename stem (e.g. "prod" for `layouts/prod.ncl`).
    pub layout: String,
    /// Package name → version for all crates in the build.
    pub packages: BTreeMap<String, String>,
    /// Wall-clock build duration (milliseconds). `None` for archives
    /// produced by older versions of the build pipeline.
    #[serde(default)]
    pub build_duration_ms: Option<u64>,
    /// Solved memory-region allocations. Source of truth for the UI's
    /// memory map on loaded firmware; `None` for older archives.
    #[serde(default)]
    pub allocations: Vec<AllocationRecord>,
    /// Raw cargo JSON messages from the build. Each entry is one ndjson
    /// line paired with the crate that emitted it. Older archives
    /// without this field deserialize to an empty vec.
    #[serde(default)]
    pub cargo_messages: Vec<CargoMessageRecord>,
}

impl BuildMetadata {
    /// Create build metadata from the current build context.
    pub fn from_build(
        build_id: &str,
        name: &str,
        config: &str,
        version: Option<&str>,
        board: &str,
        layout: &str,
        firmware_dir: &Path,
    ) -> Self {
        BuildMetadata {
            build_id: build_id.to_string(),
            built_at: iso8601_now(),
            version: version.map(|v| v.to_string()),
            name: name.to_string(),
            config: config.to_string(),
            board: board.to_string(),
            layout: layout.to_string(),
            packages: collect_package_versions(firmware_dir),
            build_duration_ms: None,
            allocations: Vec::new(),
            cargo_messages: Vec::new(),
        }
    }
}

/// Produce an ISO 8601 timestamp string from the current system time.
fn iso8601_now() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();

    // Simple UTC breakdown without pulling in chrono.
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since epoch → year/month/day (simplified, handles leap years).
    let (year, month, day) = days_to_ymd(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970;
    loop {
        let year_days = if is_leap(year) { 366 } else { 365 };
        if days < year_days {
            break;
        }
        days -= year_days;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];

    let mut month = 1;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month, days + 1)
}

fn is_leap(y: u64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// Collect package versions from the firmware Cargo.lock.
fn collect_package_versions(firmware_dir: &Path) -> BTreeMap<String, String> {
    let lock_path = firmware_dir.join("Cargo.lock");
    let Ok(contents) = std::fs::read_to_string(&lock_path) else {
        return BTreeMap::new();
    };

    let mut packages = BTreeMap::new();
    let mut current_name: Option<String> = None;

    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with("name = ") {
            current_name = line
                .strip_prefix("name = \"")
                .and_then(|s| s.strip_suffix('"'))
                .map(|s| s.to_string());
        } else if line.starts_with("version = ") {
            if let (Some(name), Some(ver)) = (
                current_name.take(),
                line.strip_prefix("version = \"")
                    .and_then(|s| s.strip_suffix('"')),
            ) {
                packages.insert(name, ver.to_string());
            }
        }
    }

    packages
}
