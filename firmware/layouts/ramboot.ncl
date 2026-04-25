let lib = import "../lib.ncl" in

fun memory =>
  # Flash partition table. The stub boots from RAM (loaded via DAP), but
  # the flash partition layout still needs to match the target it will
  # flash — that's the whole point of `FlashLayout::get_layout` on the
  # stub side. Keep the partitions identical to `prod.ncl`.
  let flash = lib.partition memory.mpi2 [
    { name = "boot",     size = lib.kiB 20 },
    { name = "firmware", size = lib.kiB 384 },
    { name = "xip_rom",  size = lib.kiB 64 },
    { name = "logs",     size = lib.miB 1 },
    { name = "fs_main",  size = lib.miB 1 },
  ] in

  {
    # RAM targets — same roles as prod. The stub's code lands in these
    # SRAM regions directly, bypassing the boot-from-flash flow.
    scratch = memory.sram_fast_dctm,    # 128K · exec · NOT retained
    fast    = memory.sram_fast,         # 128K · exec · retained
    bulk    = memory.sram_bulk,         # 256K · exec · retained — default code/data
    psram   = memory.mpi1,              # 8M   · exec · not retained

    # Flash partitions — storage only (no CPU-mapped exec window exposed).
    boot     = flash.boot     & { unmapped = true },
    firmware = flash.firmware & { unmapped = true },
    logs     = flash.logs     & { unmapped = true },
    fs_main  = flash.fs_main  & { unmapped = true },

    # Opt-in XIP-from-NOR. Discouraged.
    xip_rom = memory.mpi1,
  }
