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
/// Used to group allocations by physical location.
fn place_key(place: &Place) -> u64 {
    resolve_cpu_address(place, false).unwrap_or(place.offset.unwrap_or(0))
}

pub fn solve(config: &AppConfig) -> Result<Layout, LayoutError> {
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

    // Track cursors per place (keyed by CPU base address).
    let mut cursors: BTreeMap<u64, u64> = BTreeMap::new();

    // Fixed-size regions first
    let mut fixed: Vec<&(RegionKey, &RegionRequest)> = requests
        .iter()
        .filter(|(_, req)| req.size.is_some())
        .collect();
    fixed.sort_by(|a, b| {
        place_key(&a.1.place)
            .cmp(&place_key(&b.1.place))
            .then(b.1.size.cmp(&a.1.size))
    });

    for (key, req) in &fixed {
        if !is_cpu_mapped(&req.place) {
            acl_only.push(key.clone());
            continue;
        }

        let size = req.size.unwrap();
        let align = req.align.unwrap_or(4);

        let cpu_base = resolve_cpu_address(&req.place, false)
            .ok_or_else(|| LayoutError::NoMapping {
                owner: key.0.clone(),
                region: key.1.clone(),
            })?;

        let pk = place_key(&req.place);
        let cursor = cursors.entry(pk).or_insert(cpu_base);

        let aligned = (*cursor + align - 1) & !(align - 1);
        let end = aligned + size;
        let space_end = cpu_base + req.place.size;

        if end > space_end {
            return Err(LayoutError::OutOfSpace {
                owner: key.0.clone(),
                region: key.1.clone(),
                needed: size,
                available: space_end.saturating_sub(*cursor),
            });
        }

        *cursor = end;
        placed.insert(key.clone(), Allocation { base: aligned, size });
    }

    // Deferred regions
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

    /// Resolve deferred task regions (not kernel or bootloader) using measured sizes.
    /// Groups deferred regions by their place, allocates sequentially
    /// after existing placed regions in each place.
    pub fn resolve_deferred(
        &mut self,
        measured: &BTreeMap<RegionKey, u64>,
    ) -> Result<(), LayoutError> {
        // Group task-only deferred keys by place
        let mut by_place: BTreeMap<u64, Vec<(RegionKey, u64, u64, &Place)>> = BTreeMap::new();

        for (key, req) in &self.deferred {
            if key.0 == "kernel" || key.0 == "bootloader" { continue; }

            let size = match measured.get(key).copied() {
                Some(s) => s,
                None if key.1 == "code" => {
                    return Err(LayoutError::DeferredNotMeasured {
                        owner: key.0.clone(),
                        region: key.1.clone(),
                    });
                }
                // Data/stack regions can legitimately be empty (e.g. no
                // mutable statics means no .data segment in the ELF).
                None => 0,
            };
            let align = req.align.unwrap_or(32);
            if let Some(cpu_base) = resolve_cpu_address(&req.place, false) {
                by_place
                    .entry(cpu_base)
                    .or_default()
                    .push((key.clone(), size, align, &req.place));
            }
        }

        for (place_base, regions) in &by_place {
            let place_size = regions.first().map(|(_, _, _, p)| p.size).unwrap_or(0);
            let place_end = place_base + place_size;

            let mut cursor = self.placed.values()
                .filter(|a| a.base >= *place_base && a.base < place_end)
                .map(|a| a.base + a.size)
                .max()
                .unwrap_or(*place_base);

            for (key, size, align, _place) in regions {
                let align = *align;
                cursor = (cursor + align - 1) & !(align - 1);

                if cursor + size > place_end {
                    return Err(LayoutError::OutOfSpace {
                        owner: key.0.clone(),
                        region: key.1.clone(),
                        needed: *size,
                        available: place_end.saturating_sub(cursor),
                    });
                }

                self.placed.insert(key.clone(), Allocation {
                    base: cursor,
                    size: *size,
                });
                cursor += size;
            }
        }

        // Remove resolved task entries, keep kernel + bootloader deferred
        self.deferred.retain(|key, _| key.0 == "kernel" || key.0 == "bootloader");

        Ok(())
    }

    /// Resolve deferred bootloader regions using measured sizes.
    pub fn resolve_bootloader_deferred(
        &mut self,
        measured: &BTreeMap<RegionKey, u64>,
    ) -> Result<(), LayoutError> {
        let mut by_place: BTreeMap<u64, Vec<(RegionKey, u64, u64, &Place)>> = BTreeMap::new();

        for (key, req) in &self.deferred {
            if key.0 != "bootloader" { continue; }

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
            let align = req.align.unwrap_or(32);
            if let Some(cpu_base) = resolve_cpu_address(&req.place, false) {
                by_place
                    .entry(cpu_base)
                    .or_default()
                    .push((key.clone(), size, align, &req.place));
            }
        }

        for (place_base, regions) in &by_place {
            let place_size = regions.first().map(|(_, _, _, p)| p.size).unwrap_or(0);
            let place_end = place_base + place_size;

            let mut cursor = self.placed.values()
                .filter(|a| a.base >= *place_base && a.base < place_end)
                .map(|a| a.base + a.size)
                .max()
                .unwrap_or(*place_base);

            for (key, size, align, _place) in regions {
                let align = *align;
                cursor = (cursor + align - 1) & !(align - 1);

                if cursor + size > place_end {
                    return Err(LayoutError::OutOfSpace {
                        owner: key.0.clone(),
                        region: key.1.clone(),
                        needed: *size,
                        available: place_end.saturating_sub(cursor),
                    });
                }

                self.placed.insert(key.clone(), Allocation {
                    base: cursor,
                    size: *size,
                });
                cursor += size;
            }
        }

        self.deferred.retain(|key, _| key.0 != "bootloader");
        Ok(())
    }

    /// Resolve deferred kernel regions using measured sizes from a sizing
    /// pass, just like `resolve_deferred` does for task regions.
    pub fn resolve_kernel_deferred(
        &mut self,
        measured: &BTreeMap<RegionKey, u64>,
    ) -> Result<(), LayoutError> {
        let mut by_place: BTreeMap<u64, Vec<(RegionKey, u64, u64, &Place)>> = BTreeMap::new();

        for (key, req) in &self.deferred {
            if key.0 != "kernel" { continue; }

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
            let align = req.align.unwrap_or(32);
            if let Some(cpu_base) = resolve_cpu_address(&req.place, false) {
                by_place
                    .entry(cpu_base)
                    .or_default()
                    .push((key.clone(), size, align, &req.place));
            }
        }

        for (place_base, regions) in &by_place {
            let place_size = regions.first().map(|(_, _, _, p)| p.size).unwrap_or(0);
            let place_end = place_base + place_size;

            let mut cursor = self.placed.values()
                .filter(|a| a.base >= *place_base && a.base < place_end)
                .map(|a| a.base + a.size)
                .max()
                .unwrap_or(*place_base);

            for (key, size, align, _place) in regions {
                let align = *align;
                cursor = (cursor + align - 1) & !(align - 1);

                if cursor + size > place_end {
                    return Err(LayoutError::OutOfSpace {
                        owner: key.0.clone(),
                        region: key.1.clone(),
                        needed: *size,
                        available: place_end.saturating_sub(cursor),
                    });
                }

                self.placed.insert(key.clone(), Allocation {
                    base: cursor,
                    size: *size,
                });
                cursor += size;
            }
        }

        self.deferred.retain(|key, _| key.0 != "kernel");
        Ok(())
    }
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
