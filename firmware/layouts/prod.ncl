let lib = import "../lib.ncl" in

fun memory =>
  # Flash partitions — storage only. Each is marked `unmapped = true`
  # except `code_generic`, which is CPU-exec-mapped for XIP.
  # firmware_a and firmware_b form the A/B partition pair: the build
  # system links code for both slots, and the bootloader selects the
  # best one at boot.
  let flash = lib.partition memory.mpi2 [
    { name = "boot",       size = lib.kiB 20 },
    { name = "firmware_a", size = lib.kiB 500 },
    { name = "firmware_b", size = lib.kiB 500 },
    { name = "logs",       size = lib.miB 1 },
    { name = "fs_main",    size = lib.miB 1 },
  ] in

  {
    # RAM targets — where code and data run at runtime.
    scratch = memory.sram_fast_dctm,    # 128K · exec · NOT retained across standby
    fast    = memory.sram_fast,         # 128K · exec · retained across standby
    bulk    = memory.sram_bulk,         # 256K · exec · retained across standby — default code/data
    psram   = memory.mpi1,              # 8M   · exec · not retained — framebuf, bulk spill, XIP-OK

    # Flash partitions — storage only (no CPU-mapped exec window exposed).
    boot       = flash.boot       & { unmapped = true },
    firmware_a = flash.firmware_a & { unmapped = true },
    firmware_b = flash.firmware_b & { unmapped = true },
    logs       = flash.logs       & { unmapped = true },
    fs_main    = flash.fs_main    & { unmapped = true },

    # XIP code placement for sysmodules that don't need RAM-resident .text.
    # Canonical addresses come from firmware_a; the build system re-links
    # for firmware_b to produce the slot B image.
    code_generic = flash.firmware_a,
  }
