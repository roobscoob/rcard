let lib = import "../lib.ncl" in

fun memory =>
  # Flash partitions — storage only. Each is marked `unmapped = true`
  # except `xip_rom`, which is intentionally CPU-exec-mapped for the
  # rare case where a task wants XIP from NOR.
  let flash = lib.partition memory.mpi2 [
    { name = "boot",     size = lib.kiB 20 },
    { name = "firmware", size = lib.kiB 384 },
    { name = "xip_rom",  size = lib.kiB 64 },
    { name = "logs",     size = lib.miB 1 },
    { name = "fs_main",  size = lib.miB 1 },
  ] in

  {
    # RAM targets — where code and data run at runtime.
    scratch = memory.sram_fast_dctm,    # 128K · exec · NOT retained across standby
    fast    = memory.sram_fast,         # 128K · exec · retained across standby
    bulk    = memory.sram_bulk,         # 256K · exec · retained across standby — default code/data
    psram   = memory.mpi1,              # 8M   · exec · not retained — framebuf, bulk spill, XIP-OK

    # Flash partitions — storage only (no CPU-mapped exec window exposed).
    boot     = flash.boot     & { unmapped = true },
    firmware = flash.firmware & { unmapped = true },
    logs     = flash.logs     & { unmapped = true },
    fs_main  = flash.fs_main  & { unmapped = true },

    # Opt-in XIP-from-NOR — discouraged. Keeps the CPU exec mapping so a
    # task that explicitly targets this place will XIP. Only use for cold
    # paths (factory/DFU/self-update) that can tolerate slow flash reads.
    xip_rom = flash.xip_rom,
  }
