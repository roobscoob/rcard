## SF32LB52 chip family — shared base definition.
## Import this and merge with variant-specific overrides.

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
    trng        = { base = 0x5000F000, size = 0x1000, irqs = { irq = 69 } },
  },
}
