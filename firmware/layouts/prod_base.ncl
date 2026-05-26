let lib = import "../lib.ncl" in
{
  build = fun memory code_slot =>
    # Flash partitions — storage only. Each is marked `unmapped = true`;
    # `code_generic` is the CPU-exec-mapped partition selected by the
    # image definition (firmware_a for slot A, firmware_b for slot B).
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

      # XIP code placement — the image's target partition with CPU mappings.
      code_generic = std.record.get code_slot flash,
    }
}
