use std::collections::{BTreeMap, BTreeSet};

use crate::config::{AppConfig, Place, RegionRequest, TaskConfig};

/// A concrete allocation: a region placed at a specific CPU address.
#[derive(Debug, Clone)]
pub struct Allocation {
    pub base: u64,
    pub size: u64,
}

/// Key for an allocated region: (owner, region_name).
pub type RegionKey = (String, String);

/// Result of layout solving.
#[derive(Debug)]
pub struct Layout {
    /// Fixed-size allocations placed at CPU addresses.
    pub placed: BTreeMap<RegionKey, Allocation>,
    /// Regions that need linker sizing (no explicit size).
    pub deferred: BTreeMap<RegionKey, RegionRequest>,
    /// Regions in non-CPU-mapped places (ACL only).
    pub acl_only: Vec<RegionKey>,
}

pub fn collect_tasks(config: &AppConfig) -> BTreeMap<&str, &TaskConfig> {
    let mut tasks = BTreeMap::new();

    fn walk<'a>(task: &'a TaskConfig, out: &mut BTreeMap<&'a str, &'a TaskConfig>) {
        let name = task.crate_info.package.name.as_str();
        if out.contains_key(name) {
            return;
        }
        out.insert(name, task);
        for dep in &task.depends_on {
            walk(dep, out);
        }
    }

    for entry in &config.entries {
        walk(entry, &mut tasks);
    }

    tasks
}

/// Resolve a place to a CPU base address.
///
/// When `needs_execute` is true, prefer an executable mapping.
/// When false, prefer a non-executable mapping — this places data
/// regions into the ARM default memory map's RAM region (WBWA)
/// rather than the Code region (WT), which matters for D-cache
/// attribute agreement when the kernel accesses task memory via
/// PRIVDEFENA.
pub fn resolve_cpu_address(place: &Place, needs_execute: bool) -> Option<u64> {
    let offset = place.offset.unwrap_or(0);

    // Prefer a mapping that matches the execute requirement.
    for mapping in &place.mappings {
        let dominated = if needs_execute {
            !mapping.execute
        } else {
            mapping.execute
        };
        if dominated {
            continue;
        }
        if offset + place.size <= mapping.size {
            return Some(mapping.address + offset);
        }
    }

    // Fallback: any mapping that fits.
    for mapping in &place.mappings {
        if offset + place.size <= mapping.size {
            return Some(mapping.address + offset);
        }
    }

    None
}

fn is_cpu_mapped(place: &Place) -> bool {
    !place.unmapped && !place.mappings.is_empty()
}

/// Find which place in `config.places` contains the given CPU address.
/// Skips unmapped places. Used to map a layout-allocated base back to
/// its hosting place name.
pub(crate) fn find_place_name(config: &AppConfig, addr: u64) -> Option<String> {
    for (name, place) in &config.places {
        if place.unmapped || place.mappings.is_empty() {
            continue;
        }
        let offset = place.offset.unwrap_or(0);
        for mapping in &place.mappings {
            let start = mapping.address + offset;
            let end = start + place.size;
            if addr >= start && addr < end {
                return Some(name.clone());
            }
        }
    }
    None
}

/// Unique key for a place (by its first mapping address + offset, or size as fallback).
fn place_key(place: &Place) -> u64 {
    resolve_cpu_address(place, false).unwrap_or(place.offset.unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Cursor-based allocator
// ---------------------------------------------------------------------------

/// Per-place allocation state.
#[derive(Clone)]
struct PlaceCursor {
    /// CPU base address of this place.
    base: u64,
    /// Next free byte (advances as regions are allocated).
    cursor: u64,
    /// End of the place's address range (base + size).
    end: u64,
}

type CursorMap = BTreeMap<u64, PlaceCursor>;
/// Map from place_key to bytes reserved at the end of that place
/// (e.g. PLCB trailer space). Subtracted from the cursor's `end`.
pub type Reservations = BTreeMap<u64, u64>;

/// Bytes reserved at the end of the host place for the PLCB trailer
/// (partition table + segment table + footer). Conservative — tables
/// are tiny in practice (few segments, few partitions).
pub const PLCB_TRAILER_RESERVATION: u64 = 1024;

/// Compute trailer reservation for each place that hosts a
/// `places.bin`. One reservation per image's `code_generic` place.
/// Returns an empty map when `image_places` is empty.
pub fn compute_reservations(image_places: &[&Place]) -> Reservations {
    let mut out = BTreeMap::new();
    for place in image_places {
        if resolve_cpu_address(place, false).is_some() {
            out.insert(place_key(place), PLCB_TRAILER_RESERVATION);
        }
    }
    out
}

fn cursor_end(cpu_base: u64, place: &Place, reservations: &Reservations) -> u64 {
    let pk = place_key(place);
    let reserve = reservations.get(&pk).copied().unwrap_or(0);
    cpu_base + place.size.saturating_sub(reserve)
}

/// Seed a cursor for `place` if one doesn't exist yet.
fn ensure_cursor(place: &Place, cursors: &mut CursorMap, reservations: &Reservations) {
    if let Some(cpu_base) = resolve_cpu_address(place, false) {
        let pk = place_key(place);
        cursors.entry(pk).or_insert(PlaceCursor {
            base: cpu_base,
            cursor: cpu_base,
            end: cursor_end(cpu_base, place, reservations),
        });
    }
}

/// Advance all cursors past existing allocations that fall within their range.
fn advance_cursors_past(placed: &BTreeMap<RegionKey, Allocation>, cursors: &mut CursorMap) {
    for alloc in placed.values() {
        let alloc_end = alloc.base + alloc.size;
        for pc in cursors.values_mut() {
            if alloc.base >= pc.base && alloc.base < pc.end && alloc_end > pc.cursor {
                pc.cursor = alloc_end;
            }
        }
    }
}

/// Try to allocate `size` bytes (with `align`) in `place`.
/// Returns the allocation if it fits, None otherwise.
///
/// The cursor always tracks in the canonical (first-fit) address space
/// so that code and data regions sharing a physical device don't overlap.
/// The allocation's base is translated to the CPU mapping selected by
/// `needs_execute`.
fn try_allocate(
    place: &Place,
    size: u64,
    align: u64,
    needs_execute: bool,
    cursors: &mut CursorMap,
    reservations: &Reservations,
) -> Option<Allocation> {
    let canonical_base = resolve_cpu_address(place, false)?;
    let pk = place_key(place);
    let pc = cursors.entry(pk).or_insert(PlaceCursor {
        base: canonical_base,
        cursor: canonical_base,
        end: cursor_end(canonical_base, place, reservations),
    });
    let aligned = (pc.cursor + align - 1) & !(align - 1);
    let end = aligned + size;
    if end > pc.end {
        return None;
    }
    pc.cursor = end;

    let target_base = resolve_cpu_address(place, needs_execute)
        .unwrap_or(canonical_base);
    let offset = aligned - canonical_base;
    Some(Allocation { base: target_base + offset, size })
}

/// Remaining bytes in a place's primary region.
fn remaining_in_place(place: &Place, cursors: &CursorMap) -> u64 {
    let pk = place_key(place);
    match cursors.get(&pk) {
        Some(pc) => pc.end.saturating_sub(pc.cursor),
        None => place.size,
    }
}

// ---------------------------------------------------------------------------
// Main solver
// ---------------------------------------------------------------------------

pub fn solve(config: &AppConfig, reservations: &Reservations) -> Result<Layout, LayoutError> {
    let tasks = collect_tasks(config);

    let mut requests: Vec<(RegionKey, &RegionRequest)> = Vec::new();

    if let Some(bl) = &config.bootloader {
        for (region_name, req) in &bl.regions {
            requests.push((("bootloader".to_string(), region_name.clone()), req));
        }
    }

    for (region_name, req) in &config.kernel.regions {
        requests.push((("kernel".to_string(), region_name.clone()), req));
    }

    for (crate_name, task) in &tasks {
        for (region_name, req) in &task.regions {
            requests.push(((crate_name.to_string(), region_name.clone()), req));
        }
    }

    let mut placed: BTreeMap<RegionKey, Allocation> = BTreeMap::new();
    let mut deferred: BTreeMap<RegionKey, RegionRequest> = BTreeMap::new();
    let mut acl_only: Vec<RegionKey> = Vec::new();
    let mut cursors: CursorMap = BTreeMap::new();

    // ── Shared group coalescing ──────────────────────────────────────
    //
    // Collect requests that name the same `shared` group. Merge their
    // place/size/align — error if two members both specify a field and
    // disagree. The coalesced result must have at least place and size.

    struct SharedGroup {
        place: Option<Place>,
        size: Option<u64>,
        align: Option<u64>,
        members: Vec<RegionKey>,
    }

    let mut shared_groups: BTreeMap<String, SharedGroup> = BTreeMap::new();

    for (key, req) in &requests {
        if let Some(group) = &req.shared {
            let entry = shared_groups.entry(group.clone()).or_insert(SharedGroup {
                place: None,
                size: None,
                align: None,
                members: Vec::new(),
            });
            entry.members.push(key.clone());

            if let Some(ref req_place) = req.place {
                if let Some(ref existing) = entry.place {
                    if place_key(existing) != place_key(req_place) {
                        return Err(LayoutError::SharedConflict {
                            group: group.clone(),
                            field: "place".into(),
                            owner: key.0.clone(),
                            region: key.1.clone(),
                        });
                    }
                } else {
                    entry.place = Some(req_place.clone());
                }
            }
            if let Some(req_size) = req.size {
                if let Some(existing) = entry.size {
                    if existing != req_size {
                        return Err(LayoutError::SharedConflict {
                            group: group.clone(),
                            field: "size".into(),
                            owner: key.0.clone(),
                            region: key.1.clone(),
                        });
                    }
                } else {
                    entry.size = Some(req_size);
                }
            }
            if let Some(req_align) = req.align {
                if let Some(existing) = entry.align {
                    if existing != req_align {
                        return Err(LayoutError::SharedConflict {
                            group: group.clone(),
                            field: "align".into(),
                            owner: key.0.clone(),
                            region: key.1.clone(),
                        });
                    }
                } else {
                    entry.align = Some(req_align);
                }
            }
        }
    }

    // Allocate each shared group once and map to all members.
    for (group_name, group) in &shared_groups {
        let place = group.place.as_ref().ok_or_else(|| LayoutError::SharedMissing {
            group: group_name.clone(),
            field: "place".into(),
        })?;
        let size = group.size.ok_or_else(|| LayoutError::SharedMissing {
            group: group_name.clone(),
            field: "size".into(),
        })?;
        let align = group.align.unwrap_or(4);

        if !is_cpu_mapped(place) {
            for key in &group.members {
                acl_only.push(key.clone());
            }
            continue;
        }

        let alloc = try_allocate(place, size, align, false, &mut cursors, reservations)
            .ok_or_else(|| LayoutError::OutOfSpace(OutOfSpaceDetail {
                owner: format!("shared:{}", group_name),
                region: group_name.clone(),
                needed: size,
                available: remaining_in_place(place, &cursors),
                place_name: place.name.clone().unwrap_or_else(|| "?".into()),
                place_size: place.size,
                occupants: occupants_in_place(&placed, place),
            }))?;

        for key in &group.members {
            placed.insert(key.clone(), alloc.clone());
        }
    }

    // ── Private (non-shared) fixed-size regions ──────────────────────

    let mut fixed: Vec<&(RegionKey, &RegionRequest)> = requests
        .iter()
        .filter(|(_, req)| req.shared.is_none() && req.size.is_some())
        .collect();
    fixed.sort_by(|a, b| {
        let pa = a.1.place.as_ref().unwrap();
        let pb = b.1.place.as_ref().unwrap();
        place_key(pa)
            .cmp(&place_key(pb))
            .then(b.1.size.cmp(&a.1.size))
    });

    for (key, req) in &fixed {
        let place = req.place.as_ref().unwrap();
        if !is_cpu_mapped(place) {
            acl_only.push(key.clone());
            continue;
        }

        let size = req.size.unwrap();
        let align = req.align.unwrap_or(4);

        let alloc = try_allocate(place, size, align, req.execute, &mut cursors, reservations)
            .ok_or_else(|| LayoutError::OutOfSpace(OutOfSpaceDetail {
                owner: key.0.clone(),
                region: key.1.clone(),
                needed: size,
                available: remaining_in_place(place, &cursors),
                place_name: place.name.clone().unwrap_or_else(|| "?".into()),
                place_size: place.size,
                occupants: occupants_in_place(&placed, place),
            }))?;

        placed.insert(key.clone(), alloc);
    }

    // Deferred regions (sized later by linker).
    for (key, req) in &requests {
        if req.shared.is_some() {
            continue;
        }
        if req.size.is_none() {
            let place = req.place.as_ref().unwrap();
            if is_cpu_mapped(place) {
                deferred.insert(key.clone(), (*req).clone());
            } else {
                acl_only.push(key.clone());
            }
        }
    }

    Ok(Layout { placed, deferred, acl_only })
}

// ---------------------------------------------------------------------------
// Deferred resolution
// ---------------------------------------------------------------------------

impl Layout {
    pub fn regions_for(&self, owner: &str) -> Vec<(&str, &Allocation)> {
        self.placed
            .iter()
            .filter(|((o, _), _)| o == owner)
            .map(|((_, name), alloc)| (name.as_str(), alloc))
            .collect()
    }

    pub fn task_names(&self) -> BTreeSet<&str> {
        self.placed
            .keys()
            .chain(self.deferred.keys())
            .chain(self.acl_only.iter())
            .map(|(owner, _)| owner.as_str())
            .filter(|o| *o != "kernel")
            .collect()
    }

    /// Resolve deferred task regions (not kernel or bootloader).
    pub fn resolve_deferred(
        &mut self,
        measured: &BTreeMap<RegionKey, u64>,
        reservations: &Reservations,
    ) -> Result<(), LayoutError> {
        self.resolve_deferred_filtered(measured, reservations, |key| {
            key.0 != "kernel" && key.0 != "bootloader"
        })?;
        self.deferred.retain(|key, _| key.0 == "kernel" || key.0 == "bootloader");
        Ok(())
    }

    /// Resolve deferred bootloader regions.
    pub fn resolve_bootloader_deferred(
        &mut self,
        measured: &BTreeMap<RegionKey, u64>,
        reservations: &Reservations,
    ) -> Result<(), LayoutError> {
        self.resolve_deferred_filtered(measured, reservations, |key| key.0 == "bootloader")?;
        self.deferred.retain(|key, _| key.0 != "bootloader");
        Ok(())
    }

    /// Resolve deferred kernel regions.
    pub fn resolve_kernel_deferred(
        &mut self,
        measured: &BTreeMap<RegionKey, u64>,
        reservations: &Reservations,
    ) -> Result<(), LayoutError> {
        self.resolve_deferred_filtered(measured, reservations, |key| key.0 == "kernel")?;
        self.deferred.retain(|key, _| key.0 != "kernel");
        Ok(())
    }

    /// Shared: resolve deferred regions matching `filter`.
    fn resolve_deferred_filtered(
        &mut self,
        measured: &BTreeMap<RegionKey, u64>,
        reservations: &Reservations,
        filter: impl Fn(&RegionKey) -> bool,
    ) -> Result<(), LayoutError> {
        struct Region {
            key: RegionKey,
            size: u64,
            align: u64,
            place: Place,
            execute: bool,
        }

        let mut regions: Vec<Region> = Vec::new();
        for (key, req) in &self.deferred {
            if !filter(key) { continue; }

            let size = match measured.get(key).copied() {
                Some(s) => s,
                None if key.1 == "code" => {
                    return Err(LayoutError::DeferredNotMeasured {
                        owner: key.0.clone(),
                        region: key.1.clone(),
                    });
                }
                None => 0,
            };
            let place = match req.place {
                Some(ref p) => p,
                None => continue,
            };
            if !is_cpu_mapped(place) { continue; }

            regions.push(Region {
                key: key.clone(),
                size,
                align: req.align.unwrap_or(32),
                place: place.clone(),
                execute: req.execute,
            });
        }

        // Build cursor map: seed entries for every referenced place,
        // then advance past existing allocations.
        let mut cursors: CursorMap = BTreeMap::new();
        for r in &regions {
            ensure_cursor(&r.place, &mut cursors, reservations);
        }
        advance_cursors_past(&self.placed, &mut cursors);

        // Pack each place's regions largest-first.
        let mut sorted: Vec<&Region> = regions.iter().collect();
        sorted.sort_by(|a, b| {
            place_key(&a.place)
                .cmp(&place_key(&b.place))
                .then(b.size.cmp(&a.size))
        });

        for r in sorted {
            if r.size == 0 {
                // Region has no content, but we still need an entry
                // in `placed` so the linker script gets a valid ORIGIN
                // for the memory region. Use the current cursor position
                // as the base so the address is real. Downstream MPU
                // setup must skip zero-size allocations to avoid an
                // ARMv8-M RLAR wraparound.
                let canonical = resolve_cpu_address(&r.place, false).unwrap_or(0);
                let target = resolve_cpu_address(&r.place, r.execute).unwrap_or(canonical);
                let pk = place_key(&r.place);
                let cursor = cursors.get(&pk).map(|pc| pc.cursor).unwrap_or(canonical);
                let offset = cursor - canonical;
                self.placed.insert(r.key.clone(), Allocation { base: target + offset, size: 0 });
                continue;
            }

            let alloc = try_allocate(&r.place, r.size, r.align, r.execute, &mut cursors, reservations)
                .ok_or_else(|| LayoutError::OutOfSpace(OutOfSpaceDetail {
                    owner: r.key.0.clone(),
                    region: r.key.1.clone(),
                    needed: r.size,
                    available: remaining_in_place(&r.place, &cursors),
                    place_name: r.place.name.clone().unwrap_or_else(|| "?".into()),
                    place_size: r.place.size,
                    occupants: occupants_in_place(&self.placed, &r.place),
                }))?;

            self.placed.insert(r.key.clone(), alloc);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Canonical task ordering
// ---------------------------------------------------------------------------

/// Compute dependency depth for each task. Leaf tasks (no dependencies)
/// get depth 1; a task that depends on a depth-N task gets depth N+1.
/// Tasks not found in `all_tasks` get depth 0.
pub fn dep_depths(all_tasks: &BTreeMap<&str, &TaskConfig>) -> BTreeMap<String, u8> {
    fn dep_depth(
        name: &str,
        all: &BTreeMap<&str, &TaskConfig>,
        cache: &mut BTreeMap<String, u8>,
    ) -> u8 {
        if let Some(&d) = cache.get(name) {
            return d;
        }
        let task = match all.get(name) {
            Some(t) => t,
            None => {
                cache.insert(name.to_string(), 0);
                return 0;
            }
        };
        let max_dep = task
            .depends_on
            .iter()
            .map(|dep| dep_depth(&dep.crate_info.package.name, all, cache))
            .max()
            .unwrap_or(0);
        let d = max_dep.saturating_add(1);
        cache.insert(name.to_string(), d);
        d
    }

    let mut depths = BTreeMap::new();
    for &name in all_tasks.keys() {
        dep_depth(name, all_tasks, &mut depths);
    }
    depths
}

/// Return task names in canonical build order:
/// **(workgroup ASC, dep_depth ASC, name ASC)**.
///
/// - Lower workgroup = more important (kernel before sysmodule before app).
/// - Lower dep_depth = more important within a workgroup (a depended-on
///   server runs before its client).
/// - Alphabetical name as final tiebreaker.
///
/// This single ordering is used for task table slot indices, codegen
/// `task_indices`, and kernel scheduling priorities.  All three must agree.
pub fn ordered_task_names<'a>(all_tasks: &BTreeMap<&'a str, &TaskConfig>) -> Vec<&'a str> {
    let depths = dep_depths(all_tasks);
    let mut entries: Vec<(&str, u32, u8)> = all_tasks
        .iter()
        .map(|(&name, task)| {
            let dd = depths.get(name).copied().unwrap_or(0);
            (name, task.priority, dd)
        })
        .collect();
    entries.sort_by(|a, b| {
        a.1.cmp(&b.1) // workgroup ASC
            .then(a.2.cmp(&b.2)) // dep_depth ASC (leaves first)
            .then(a.0.cmp(&b.0)) // name ASC
    });
    entries.into_iter().map(|(name, _, _)| name).collect()
}

// ---------------------------------------------------------------------------
// Out-of-space detail
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PlaceOccupant {
    pub owner: String,
    pub region: String,
    pub size: u64,
}

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn occupants_in_place(
    placed: &BTreeMap<RegionKey, Allocation>,
    place: &Place,
) -> Vec<PlaceOccupant> {
    let offset = place.offset.unwrap_or(0);
    let mut out: Vec<PlaceOccupant> = placed
        .iter()
        .filter(|(_, alloc)| {
            alloc.size > 0
                && place.mappings.iter().any(|m| {
                    let start = m.address + offset;
                    let end = start + place.size;
                    alloc.base >= start && alloc.base < end
                })
        })
        .map(|((owner, region), alloc)| PlaceOccupant {
            owner: owner.clone(),
            region: region.clone(),
            size: alloc.size,
        })
        .collect();
    out.sort_by(|a, b| b.size.cmp(&a.size));
    out
}

#[derive(Debug)]
pub struct OutOfSpaceDetail {
    pub owner: String,
    pub region: String,
    pub needed: u64,
    pub available: u64,
    pub place_name: String,
    pub place_size: u64,
    pub occupants: Vec<PlaceOccupant>,
}

impl std::fmt::Display for OutOfSpaceDetail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}.{}: need {}, only {} available in {} ({} total)",
            self.owner,
            self.region,
            human_size(self.needed),
            human_size(self.available),
            self.place_name,
            human_size(self.place_size),
        )?;
        if !self.occupants.is_empty() {
            let name_w = self
                .occupants
                .iter()
                .map(|o| o.owner.len() + 1 + o.region.len())
                .max()
                .unwrap_or(10);
            for o in &self.occupants {
                let label = format!("{}.{}", o.owner, o.region);
                write!(f, "\n    {:<w$}  {:>10}", label, human_size(o.size), w = name_w)?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum LayoutError {
    NoMapping { owner: String, region: String },
    OutOfSpace(OutOfSpaceDetail),
    DeferredNotMeasured { owner: String, region: String },
    SharedConflict { group: String, field: String, owner: String, region: String },
    SharedMissing { group: String, field: String },
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoMapping { owner, region } => {
                write!(f, "{owner}.{region}: place has no CPU mapping")
            }
            Self::OutOfSpace(detail) => detail.fmt(f),
            Self::DeferredNotMeasured { owner, region } => {
                write!(f, "{owner}.{region}: deferred region was not measured")
            }
            Self::SharedConflict { group, field, owner, region } => {
                write!(f, "shared group \"{group}\": {owner}.{region} conflicts on {field}")
            }
            Self::SharedMissing { group, field } => {
                write!(f, "shared group \"{group}\": no member specifies {field}")
            }
        }
    }
}

impl std::error::Error for LayoutError {}
