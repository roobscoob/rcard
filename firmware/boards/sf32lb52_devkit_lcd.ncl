## SF32LB52 Devkit LCD — SF32LB525UC6 (512KB SRAM)

let chip = import "../chips/sf32lb525uc6.ncl" in

{
  target = chip.target,
  vector_table_size = chip.vector_table_size,
  kernel = import "../kernels/sf32lb52/kernel.ncl",
  address_spaces = chip.address_spaces,
  peripheral_map = chip.peripherals,
  pins = chip.pins,
  pin_assignments = {},
}
