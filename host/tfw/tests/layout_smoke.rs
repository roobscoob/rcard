use std::path::Path;

fn firmware_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../firmware"))
}

#[test]
fn solve_fob_layout() {
    let config = tfw::config::load(
        firmware_dir(), "fob.ncl", "boards/bentoboard.ncl", "layouts/prod.ncl",
    ).expect("failed to load config");

    let layout = tfw::layout::solve(&config).expect("layout failed");

    // Kernel stack placed
    let kernel_stack = &layout.placed[&("kernel".to_string(), "stack".to_string())];
    assert_eq!(kernel_stack.size, 1024);
    // Should be in sram_fast_dctm range
    assert!(kernel_stack.base >= 0x20000000);

    // Kernel code is deferred
    assert!(layout.deferred.contains_key(&("kernel".to_string(), "code".to_string())));

    // Bootloader stack placed
    let bl_stack = &layout.placed[&("bootloader".to_string(), "stack".to_string())];
    assert_eq!(bl_stack.size, 256);
    assert!(bl_stack.base >= 0x20000000); // SRAM

    // Bootloader code is deferred
    assert!(layout.deferred.contains_key(&("bootloader".to_string(), "code".to_string())));

    // fob stack
    let fob_stack = &layout.placed[&("fob".to_string(), "stack".to_string())];
    assert_eq!(fob_stack.size, 8192);
}

#[test]
fn allocations_dont_overlap() {
    let config = tfw::config::load(
        firmware_dir(), "fob.ncl", "boards/bentoboard.ncl", "layouts/prod.ncl",
    ).expect("failed to load config");

    let layout = tfw::layout::solve(&config).expect("layout failed");

    // Group by base address range and check no overlaps
    let mut ranges: Vec<(u64, u64, String)> = layout
        .placed
        .iter()
        .map(|((owner, region), alloc)| {
            (alloc.base, alloc.base + alloc.size, format!("{owner}.{region}"))
        })
        .collect();
    ranges.sort();

    for window in ranges.windows(2) {
        assert!(
            window[0].1 <= window[1].0,
            "overlap: {} [{:#x}, {:#x}) and {} [{:#x}, {:#x})",
            window[0].2, window[0].0, window[0].1,
            window[1].2, window[1].0, window[1].1,
        );
    }
}

#[test]
fn only_reachable_tasks_are_allocated() {
    let config = tfw::config::load(
        firmware_dir(), "fob.ncl", "boards/bentoboard.ncl", "layouts/prod.ncl",
    ).expect("failed to load config");

    let layout = tfw::layout::solve(&config).expect("layout failed");
    let names = layout.task_names();
    assert!(names.contains("fob"));
    assert!(names.contains("sysmodule_log"));
    assert!(!names.contains("stub"));
}
