let lib = import "../lib.ncl" in

fun memory =>
  # Flash partition table. The stub boots from RAM (loaded via DAP), but
  # the flash partition layout still needs to match the target it will
  # flash — that's the whole point of `FlashLayout::get_layout` on the
  # stub side. Keep the partitions identical to `prod.ncl`.
  let flash = lib.partition memory.mpi2 [
    { name = "boot",       size = lib.kiB 20 },
    { name = "firmware_a", size = lib.kiB 500 },
    { name = "firmware_b", size = lib.kiB 500 },
    { name = "logs",       size = lib.miB 1 },
    { name = "fs_main",    size = lib.miB 1 },
  ] in

  {
    # RAM targets — same roles as prod. The stub's code lands in these
    # SRAM regions directly, bypassing the boot-from-flash flow.
    scratch = memory.sram_fast_dctm,    # 128K · exec · NOT retained
    fast    = memory.sram_fast,         # 128K · exec · retained
    bulk    = memory.sram_bulk,         # 256K · exec · retained — default code/data
    psram   = memory.mpi1,              # 8M   · exec · not retained

    # Flash partitions — storage only (no CPU-mapped exec window exposed).
    boot       = flash.boot       & { unmapped = true },
    firmware_a = flash.firmware_a & { unmapped = true },
    firmware_b = flash.firmware_b & { unmapped = true },
    logs       = flash.logs       & { unmapped = true },
    fs_main    = flash.fs_main    & { unmapped = true },

    # XIP code placement for sysmodules that don't need RAM-resident .text
    code_generic = memory.sram_bulk,
  }
