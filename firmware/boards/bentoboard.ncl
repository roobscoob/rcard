## Bentoboard — SF32LB525UC6 + 32MB NOR flash + 32MB PSRAM

let chip = import "../chips/sf32lb525uc6.ncl" in
let kiB = fun n => n * 1024 in
let miB = fun n => n * 1024 * 1024 in

{
  target = chip.target,
  vector_table_size = chip.vector_table_size,
  kernel = import "../kernels/sf32lb52/kernel.ncl",
  bootloader = import "../bootloader/bootloader.ncl",

  memory = std.record.insert "mpi2" {
      size = miB 8,
      mappings = [
        { address = 0x12000000, size = miB 224, read = true, execute = true },
        { address = 0x62000000, size = miB 992, read = true },
      ],
    } chip.memory,

  peripheral_map = chip.peripherals,
}
