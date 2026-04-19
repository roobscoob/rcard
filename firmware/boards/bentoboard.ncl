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
      geometry = {
        erase_size = 4096,
        program_size = 256,
        read_size = 1,
      },
      mappings = [
        { address = 0x12000000, size = miB 224, read = true, execute = true },
        { address = 0x62000000, size = miB 992, read = true },
      ],
    } chip.memory,

  peripheral_map = chip.peripherals,
  pins = chip.pins,

  pin_assignments = {
    usart1 = { rx = "PA18", tx = "PA19" },
    usart2 = { tx = "PA09", rx = "PA10" },
    usb    = { dp = "PA35", dm = "PA36" },
    lcdc   = { rstb = "PA00", cs = "PA03", clk = "PA04", dio0 = "PA05", dio1 = "PA06" },
    mpi2   = { cs = "PA12", dio1 = "PA13", dio2 = "PA14", dio0 = "PA15", clk = "PA16", dio3 = "PA17" },
    i2c1   = { sda = "PA42", scl = "PA41" },
    i2c2   = { sda = "PA32", scl = "PA33" },
    i2c3   = { sda = "PA30", scl = "PA31" },
    i2s1   = { sdo = "PA25" },
    spi1   = { dio = "PA24" },
    spi2   = { dio = "PA37", di = "PA38", clk = "PA39", cs = "PA40" },
    atim1  = { ch1 = "PA07" },
    display_en = { pin = "PA02", direction = "out" },
    ws2812_en  = { pin = "PA28", direction = "out" },
    haptic_en  = { pin = "PA08", direction = "out" },
    accel_int  = { pin = "PA43", direction = "in" },
    rfid_wake  = { pin = "PA26", direction = "in" },
    touch_int  = { pin = "PA27", direction = "in" },
    nfc_int    = { pin = "PA44", direction = "in" },
    ovp_fault  = { pin = "PA01", direction = "in" },
  },
}
