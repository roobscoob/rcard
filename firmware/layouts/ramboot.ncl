let lib = import "../lib.ncl" in

fun memory =>
  let flash = lib.partition memory.mpi2 [
    { name = "sec_config", size = lib.kiB 20 },
    { name = "bootloader", size = lib.kiB 16 },
    { name = "image",      size = lib.kiB 368 },
    { name = "logs",       size = lib.miB 1 },
    { name = "fs_main",    size = lib.miB 1 },
  ] in

  {
    kernel_ram = memory.sram_bulk,
    task_ram   = memory.sram_bulk,
    image      = memory.sram_bulk,
    sec_config = flash.sec_config,
    bootloader = flash.bootloader,
    logs       = flash.logs & { unmapped = true },
    fs_main    = flash.fs_main & { unmapped = true },
    framebuf   = memory.mpi1,
  }
