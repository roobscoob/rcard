## SF32LB52 chip family — shared base definition.
## Import this and merge with variant-specific overrides.

let cap = fun k sigs => { kind = k, signals = sigs } in
let cap_inst = fun k inst sigs => { kind = k, instance = inst, signals = sigs } in
let cap_mode = fun k m sigs => { kind = k, mode = m, signals = sigs } in

let gpio_signals = ["any"] in
let usart_signals = ["tx", "rx", "cts", "rts"] in
let i2c_signals = ["sda", "scl"] in
let gptim_signals = ["ch1", "ch2", "ch3", "ch4", "etr"] in
let lptim_signals = ["in", "out", "etr"] in
let atim_signals = ["ch1", "ch2", "ch3", "ch4", "ch1n", "ch2n", "ch3n", "bk", "bk2", "etr"] in

# Common capabilities shared by most pins
let common_caps = [
  cap "gpio" gpio_signals,
  cap "usart" usart_signals,
  cap "i2c" i2c_signals,
  cap "gptim" gptim_signals,
  cap "lptim" lptim_signals,
  cap "atim" atim_signals,
] in

let pin = fun pull extra => {
  default_pull = pull,
  supports = common_caps @ extra,
} in

let kiB = fun n => n * 1024 in
let miB = fun n => n * 1024 * 1024 in

{
  target = "thumbv8m.main-none-eabihf",
  vector_table_size = 0x1CC,

  memory = {
    sram_fast_dctm = { size = kiB 128, mappings = [
      { address = 0x20000000, size = kiB 128, read = true, write = true, execute = true },
    ]},
    sram_fast = { size = kiB 128, mappings = [
      { address = 0x20020000, size = kiB 128, read = true, write = true, execute = true },
    ]},
    sram_bulk = { size = kiB 256, mappings = [
      { address = 0x20040000, size = kiB 256, read = true, write = true, execute = true },
    ]},
  },

  peripherals = {
    usart1      = { base = 0x50084000, size = 0x1000, irqs = { irq = 59 }, renode = { model = "UART.STM32F7_USART", properties = { frequency = 240000000 } } },
    usart2      = { base = 0x50085000, size = 0x1000, irqs = { irq = 74 }, renode = { model = "UART.STM32F7_USART", properties = { frequency = 240000000 } } },
    usart3      = { base = 0x50086000, size = 0x1000, irqs = { irq = 95 }, renode = { model = "UART.STM32F7_USART", properties = { frequency = 240000000 } } },
    mpi1        = { base = 0x50041000, size = 0x1000, renode = { model = "SPI.SF32LB52_MPI" } },
    mpi2        = { base = 0x50042000, size = 0x1000, renode = { model = "SPI.SF32LB52_MPI" } },
    rcc         = { base = 0x50000000, size = 0x1000, renode = { model = "Miscellaneous.SF32LB52_HPSYS_RCC" } },
    gpios       = { base = 0x500A0000, size = 0x1000, irqs = { irq = 84 } },
    pinctrl     = { base = 0x50003000, size = 0x1000 },
    syscon      = { base = 0x5000B000, size = 0x1000 },
    aon_syscon  = { base = 0x500C0000, size = 0x1000, renode = { model = "Miscellaneous.SF32LB52_HPSYS_AON" } },
    pmuc_syscon = { base = 0x500CA000, size = 0x1000 },
    lcdc        = { base = 0x50008000, size = 0x128, renode = { model = "Display.SF32LB52_LCDC" } },
    sdmmc1      = { base = 0x50045000, size = 0x400, irqs = { irq = 57 } },
    usbc        = { base = 0x50047000, size = 0x1000, irqs = { irq = 92 } },
    rtc         = { base = 0x500CB000, size = 0x78, renode = { model = "Timers.SF32LB52_RTC" } },
    efusec      = { base = 0x5000C000, size = 0x1000 },
  },

  pins = {
    PA00 = pin "down" [cap_mode "lcdc" "spi" ["rstb"], cap_mode "lcdc" "8080" ["rstb"]],
    PA01 = pin "down" [],
    PA02 = pin "down" [cap_mode "lcdc" "spi" ["te"], cap_inst "i2s" 1 ["mclk"], cap_mode "lcdc" "jdi" ["b2"], cap_mode "lcdc" "8080" ["te"]],
    PA03 = pin "up"   [cap_mode "lcdc" "spi" ["cs"], cap_inst "i2s" 1 ["sdo"], cap_mode "lcdc" "jdi" ["b1"], cap_mode "lcdc" "8080" ["cs"]],
    PA04 = pin "down" [cap_mode "lcdc" "spi" ["clk"], cap_inst "i2s" 1 ["sdi"], cap_mode "lcdc" "jdi" ["g1"], cap_mode "lcdc" "8080" ["wr"]],
    PA05 = pin "down" [cap_mode "lcdc" "spi" ["dio0"], cap_inst "i2s" 1 ["bck"], cap_mode "lcdc" "jdi" ["r1"], cap_mode "lcdc" "8080" ["rd"]],
    PA06 = pin "down" [cap_mode "lcdc" "spi" ["dio1"], cap_inst "i2s" 1 ["lrck"], cap_mode "lcdc" "jdi" ["hst"], cap_mode "lcdc" "8080" ["dc"]],
    PA07 = pin "down" [cap_mode "lcdc" "spi" ["dio2"], cap_inst "pdm" 1 ["clk"], cap_mode "lcdc" "jdi" ["enb"], cap_mode "lcdc" "8080" ["dio0"]],
    PA08 = pin "down" [cap_mode "lcdc" "spi" ["dio3"], cap_inst "pdm" 1 ["data"], cap_mode "lcdc" "jdi" ["vst"], cap_mode "lcdc" "8080" ["dio1"]],
    PA09 = pin "down" [],
    PA10 = pin "up" [],
    PA11 = pin "up"   [],
    PA12 = pin "up"   [cap_inst "mpi" 2 ["cs"], cap_inst "sd" 1 ["dio2"]],
    PA13 = pin "down" [cap_inst "mpi" 2 ["dio1"], cap_inst "sd" 1 ["dio3"]],
    PA14 = pin "up"   [cap_inst "mpi" 2 ["dio2"], cap_inst "sd" 1 ["clk"]],
    PA15 = pin "down" [cap_inst "mpi" 2 ["dio0"], cap_inst "sd" 1 ["cmd"]],
    PA16 = pin "down" [cap_inst "mpi" 2 ["clk"], cap_inst "sd" 1 ["dio0"]],
    PA17 = pin "up"   [cap_inst "mpi" 2 ["dio3"], cap_inst "sd" 1 ["dio1"]],
    PA18 = pin "up"   [cap "swd" ["dio"]],
    PA19 = pin "none" [cap "swd" ["clk"]],
    PA20 = pin "down" [],
    PA21 = pin "down" [],
    PA22 = pin "none" [cap_inst "pdm" 1 ["clk"], cap "xtal32k" ["xi"]],
    PA23 = pin "none" [cap_inst "pdm" 1 ["data"], cap "xtal32k" ["xo"]],
    PA24 = pin "down" [cap_inst "spi" 1 ["dio"], cap_inst "i2s" 1 ["mclk"], cap "wkup" ["pin0"]],
    PA25 = pin "down" [cap_inst "spi" 1 ["di"], cap_inst "i2s" 1 ["sdo"], cap "xtal32k" ["ext"], cap "wkup" ["pin1"]],
    PA26 = pin "up"   [cap "wkup" ["pin2"]],
    PA27 = pin "up"   [cap "wkup" ["pin3"]],
    PA28 = pin "down" [cap_inst "spi" 1 ["clk"], cap_inst "i2s" 1 ["sdi"], cap "gpadc" ["ch0"]],
    PA29 = pin "down" [cap_inst "spi" 1 ["cs"], cap_inst "i2s" 1 ["bck"], cap "gpadc" ["ch1"]],
    PA30 = pin "down" [cap "efuse" ["pwr"], cap_inst "i2s" 1 ["lrck"], cap "gpadc" ["ch2"]],
    PA31 = pin "down" [cap "gpadc" ["ch3"]],
    PA32 = pin "down" [cap "gpadc" ["ch4"]],
    PA33 = pin "down" [cap "gpadc" ["ch5"]],
    PA34 = pin "down" [cap "gpadc" ["ch6"], cap "wkup" ["pin10"]],
    PA35 = pin "down" [cap "usb11" ["dp"], cap "wkup" ["pin11"]],
    PA36 = pin "down" [cap "usb11" ["dm"], cap "wkup" ["pin12"]],
    PA37 = pin "down" [cap_inst "spi" 2 ["dio"], cap_mode "lcdc" "8080" ["dio2"], cap "wkup" ["pin13"]],
    PA38 = pin "down" [cap_inst "spi" 2 ["di"], cap "wkup" ["pin14"]],
    PA39 = pin "up"   [cap_inst "spi" 2 ["clk"], cap_mode "lcdc" "jdi" ["vck"], cap_mode "lcdc" "8080" ["dio3"], cap "wkup" ["pin15"]],
    PA40 = pin "up"   [cap_inst "spi" 2 ["cs"], cap_mode "lcdc" "jdi" ["xrst"], cap_mode "lcdc" "8080" ["dio4"], cap "wkup" ["pin16"]],
    PA41 = pin "up"   [cap_mode "lcdc" "jdi" ["hck"], cap_mode "lcdc" "8080" ["dio5"], cap "wkup" ["pin17"]],
    PA42 = pin "up"   [cap_mode "lcdc" "jdi" ["r2"], cap_mode "lcdc" "8080" ["dio6"], cap "wkup" ["pin18"]],
    PA43 = pin "down" [cap_mode "lcdc" "jdi" ["g2"], cap_mode "lcdc" "8080" ["dio7"], cap "wkup" ["pin19"]],
    PA44 = pin "down" [cap "wkup" ["pin20"]],
  },
}
