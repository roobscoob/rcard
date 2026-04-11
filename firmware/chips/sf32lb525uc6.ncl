## SF32LB525UC6 — base SF32LB52 + 8MB PSRAM on MPI1

let base = import "sf32lb52.ncl" in
let miB = fun n => n * 1024 * 1024 in

{
  target = base.target,
  vector_table_size = base.vector_table_size,
  peripherals = base.peripherals,
  pins = base.pins,
  memory = std.record.insert "mpi1" {
    size = miB 8,
    block_size = 1,
    mappings = [
      { address = 0x10000000, size = miB 8, read = true, write = true, execute = true },
      { address = 0x60000000, size = miB 8, read = true, write = true },
    ],
  } base.memory,
}
