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
pub fn resolve_cpu_address(place: &Place, needs_execute: bool) -> Option<u64> {
    let offset = place.offset.unwrap_or(0);

    for mapping in &place.mappings {
        if needs_execute && !mapping.execute {
            continue;
        }
        if offset + place.size <= mapping.size {
            return Some(mapping.address + offset);
        }
    }

    // Fallback: any mapping
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

/// Unique key for a place (by its first mapping address + offset, or size as fallback).
fn place_key(place: &Place) -> u64 {
    resolve_cpu_address(place, false).unwrap_or(place.offset.unwrap_or(0))
}

fn has_alternatives(place: &Place) -> bool {
    !place.alternatives.is_empty()
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

/// Compute trailer reservation for the place that hosts `places.bin`.
/// Convention: the place named `image` hosts the file. Returns an
/// empty map when no such place exists (e.g. RAM-boot configs).
pub fn compute_reservations(config: &AppConfig) -> Reservations {
    let mut out = BTreeMap::new();
    if let Some(host) = config.places.get("image") {
        if resolve_cpu_address(host, false).is_some() {
            out.insert(place_key(host), PLCB_TRAILER_RESERVATION);
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
fn try_allocate(
    place: &Place,
    size: u64,
    align: u64,
    cursors: &mut CursorMap,
    reservations: &Reservations,
) -> Option<Allocation> {
    let cpu_base = resolve_cpu_address(place, false)?;
    let pk = place_key(place);
    let pc = cursors.entry(pk).or_insert(PlaceCursor {
        base: cpu_base,
        cursor: cpu_base,
        end: cursor_end(cpu_base, place, reservations),
    });
    let aligned = (pc.cursor + align - 1) & !(align - 1);
    let end = aligned + size;
    if end > pc.end {
        return None;
    }
    pc.cursor = end;
    Some(Allocation { base: aligned, size })
}

/// Try primary place, then each alternative in order.
fn try_allocate_any(
    place: &Place,
    size: u64,
    align: u64,
    cursors: &mut CursorMap,
    reservations: &Reservations,
) -> Option<Allocation> {
    if let Some(alloc) = try_allocate(place, size, align, cursors, reservations) {
        return Some(alloc);
    }
    for alt in &place.alternatives {
        if let Some(alloc) = try_allocate(alt, size, align, cursors, reservations) {
            return Some(alloc);
        }
    }
    None
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

    // Partition fixed-size requests into single-place and flexible.
    let fixed: Vec<&(RegionKey, &RegionRequest)> = requests
        .iter()
        .filter(|(_, req)| req.size.is_some())
        .collect();

    let mut single: Vec<&(RegionKey, &RegionRequest)> = fixed
        .iter()
        .filter(|(_, req)| !has_alternatives(&req.place))
        .copied()
        .collect();
    single.sort_by(|a, b| {
        place_key(&a.1.place)
            .cmp(&place_key(&b.1.place))
            .then(b.1.size.cmp(&a.1.size))
    });

    let mut flexible: Vec<&(RegionKey, &RegionRequest)> = fixed
        .iter()
        .filter(|(_, req)| has_alternatives(&req.place))
        .copied()
        .collect();
    flexible.sort_by(|a, b| b.1.size.cmp(&a.1.size));

    // Phase 1: single-place fixed-size regions (no fallback).
    for (key, req) in &single {
        if !is_cpu_mapped(&req.place) {
            acl_only.push(key.clone());
            continue;
        }

        let size = req.size.unwrap();
        let align = req.align.unwrap_or(4);

        let alloc = try_allocate(&req.place, size, align, &mut cursors, reservations)
            .ok_or_else(|| LayoutError::OutOfSpace {
                owner: key.0.clone(),
                region: key.1.clone(),
                needed: size,
                available: remaining_in_place(&req.place, &cursors),
            })?;

        placed.insert(key.clone(), alloc);
    }

    // Phase 2: flexible fixed-size regions (try alternatives on overflow).
    for (key, req) in &flexible {
        if !is_cpu_mapped(&req.place) {
            acl_only.push(key.clone());
            continue;
        }

        let size = req.size.unwrap();
        let align = req.align.unwrap_or(4);

        let alloc = try_allocate_any(&req.place, size, align, &mut cursors, reservations)
            .ok_or_else(|| LayoutError::OutOfSpace {
                owner: key.0.clone(),
                region: key.1.clone(),
                needed: size,
                available: remaining_in_place(&req.place, &cursors),
            })?;

        placed.insert(key.clone(), alloc);
    }

    // Deferred regions (sized later by linker).
    for (key, req) in &requests {
        if req.size.is_none() {
            if is_cpu_mapped(&req.place) {
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
            if !is_cpu_mapped(&req.place) { continue; }

            regions.push(Region {
                key: key.clone(),
                size,
                align: req.align.unwrap_or(32),
                place: req.place.clone(),
            });
        }

        // Build cursor map: seed entries for every referenced place,
        // then advance past existing allocations.
        let mut cursors: CursorMap = BTreeMap::new();
        for r in &regions {
            ensure_cursor(&r.place, &mut cursors, reservations);
            for alt in &r.place.alternatives {
                ensure_cursor(alt, &mut cursors, reservations);
            }
        }
        advance_cursors_past(&self.placed, &mut cursors);

        // Phase 1: single-place deferred regions.
        let mut single: Vec<&Region> = regions
            .iter()
            .filter(|r| !has_alternatives(&r.place))
            .collect();
        single.sort_by(|a, b| {
            place_key(&a.place)
                .cmp(&place_key(&b.place))
                .then(b.size.cmp(&a.size))
        });

        // Phase 2: flexible deferred regions (largest first).
        let mut flexible: Vec<&Region> = regions
            .iter()
            .filter(|r| has_alternatives(&r.place))
            .collect();
        flexible.sort_by(|a, b| b.size.cmp(&a.size));

        for r in single.iter().chain(flexible.iter()) {
            if r.size == 0 {
                // Region has no content, but we still need an entry
                // in `placed` so the linker script gets a valid ORIGIN
                // for the memory region. Use the current cursor position
                // as the base so the address is real. Downstream MPU
                // setup must skip zero-size allocations to avoid an
                // ARMv8-M RLAR wraparound.
                let base = resolve_cpu_address(&r.place, false).unwrap_or(0);
                let pk = place_key(&r.place);
                let cursor = cursors.get(&pk).map(|pc| pc.cursor).unwrap_or(base);
                self.placed.insert(r.key.clone(), Allocation { base: cursor, size: 0 });
                continue;
            }

            let alloc = try_allocate_any(&r.place, r.size, r.align, &mut cursors, reservations)
                .ok_or_else(|| LayoutError::OutOfSpace {
                    owner: r.key.0.clone(),
                    region: r.key.1.clone(),
                    needed: r.size,
                    available: remaining_in_place(&r.place, &cursors),
                })?;

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

#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("{owner}.{region}: place has no CPU mapping")]
    NoMapping { owner: String, region: String },
    #[error("{owner}.{region}: need {needed} bytes, {available} available")]
    OutOfSpace { owner: String, region: String, needed: u64, available: u64 },
    #[error("{owner}.{region}: deferred region was not measured")]
    DeferredNotMeasured { owner: String, region: String },
}
