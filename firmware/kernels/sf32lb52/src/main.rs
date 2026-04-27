#![no_std]
#![no_main]

use cortex_m_rt::entry;

// The PAC's __INTERRUPTS is [Vector; 0] (SVD lacks interrupt definitions).
// Provide a proper vector table with 99 entries for the SF32LB52.
#[derive(Copy, Clone)]
#[repr(C)]
pub union Vector {
    handler: unsafe extern "C" fn(),
    reserved: u32,
}

unsafe impl Sync for Vector {}

extern "C" {
    fn DefaultHandler();
    static __reset_vector: u32;
}

#[repr(C, align(128))]
pub struct VectorTable([Vector; 99]);

#[link_section = ".vector_table.interrupts"]
#[no_mangle]
pub static __INTERRUPTS: VectorTable = VectorTable(
    [Vector {
        handler: DefaultHandler,
    }; 99],
);

/// Apply all bentoboard pin assignments.
///
/// # Safety
///
/// Writes to HPSYS_PINMUX PAD registers and HPSYS_CFG PINR registers.
/// Must be called before peripherals that depend on pin muxing.
pub unsafe fn apply_pin_config() {
    /// Read-modify-write: clear `mask` bits then set `val` bits.
    #[inline(always)]
    unsafe fn rmw(addr: u32, mask: u32, val: u32) {
        let p = addr as *mut u32;
        p.write_volatile((p.read_volatile() & !mask) | val);
    }

    // PA00 -> lcdc spi rstb (FSEL=1, pull=down)
    rmw(0x5000_3034, 0x7F, 0x11);
    // PA01 -> ovp_fault gpio in (FSEL=0, pull=down, IE)
    rmw(0x5000_3038, 0x7F, 0x50);
    // PA02 -> display_en gpio out (FSEL=0, pull=down)
    rmw(0x5000_303C, 0x7F, 0x10);
    // PA03 -> lcdc spi cs (FSEL=1, pull=up)
    rmw(0x5000_3040, 0x7F, 0x31);
    // PA04 -> lcdc spi clk (FSEL=1, pull=down)
    rmw(0x5000_3044, 0x7F, 0x11);
    // PA05 -> lcdc spi dio0 (FSEL=1, pull=down)
    rmw(0x5000_3048, 0x7F, 0x11);
    // PA06 -> lcdc spi dio1 (FSEL=1, pull=down)
    rmw(0x5000_304C, 0x7F, 0x11);
    // PA07 -> atim1 ch1 (FSEL=5, pull=down)
    rmw(0x5000_3050, 0x7F, 0x15);
    rmw(0x5000_B078, 0x3F, 0x07); // PINR: atim1 ch1 = PA07
    // PA08 -> haptic_en gpio out (FSEL=0, pull=down)
    rmw(0x5000_3054, 0x7F, 0x10);
    // PA09 -> usart2 tx (FSEL=4, pull=down)
    rmw(0x5000_3058, 0x7F, 0x14);
    rmw(0x5000_B05C, 0x3F, 0x09); // PINR: usart2 tx = PA09
    // PA10 -> usart2 rx (FSEL=4, pull=up, IE)
    rmw(0x5000_305C, 0x7F, 0x74);
    rmw(0x5000_B05C, 0x3F00, 0x0A00); // PINR: usart2 rx = PA10
    // PA12 -> mpi2 cs (FSEL=1, pull=up)
    rmw(0x5000_3064, 0x7F, 0x31);
    // PA13 -> mpi2 dio1 (FSEL=1, pull=down, IE)
    rmw(0x5000_3068, 0x7F, 0x51);
    // PA14 -> mpi2 dio2 (FSEL=1, pull=up, IE)
    rmw(0x5000_306C, 0x7F, 0x71);
    // PA15 -> mpi2 dio0 (FSEL=1, pull=down, IE)
    rmw(0x5000_3070, 0x7F, 0x51);
    // PA16 -> mpi2 clk (FSEL=1, pull=down)
    rmw(0x5000_3074, 0x7F, 0x11);
    // PA17 -> mpi2 dio3 (FSEL=1, pull=up, IE)
    rmw(0x5000_3078, 0x7F, 0x71);
    // PA18 -> usart1 rx (FSEL=4, pull=up, IE)
    rmw(0x5000_307C, 0x7F, 0x74);
    rmw(0x5000_B058, 0x3F00, 0x1200); // PINR: usart1 rx = PA18
    // PA19 -> usart1 tx (FSEL=4, pull=none)
    rmw(0x5000_3080, 0x7F, 0x04);
    rmw(0x5000_B058, 0x3F, 0x13); // PINR: usart1 tx = PA19
    // PA24 -> spi1 dio (FSEL=2, pull=down)
    rmw(0x5000_3094, 0x7F, 0x12);
    // PA25 -> i2s1 sdo (FSEL=3, pull=down)
    rmw(0x5000_3098, 0x7F, 0x13);
    // PA26 -> rfid_wake gpio in (FSEL=0, pull=up, IE)
    rmw(0x5000_309C, 0x7F, 0x70);
    // PA27 -> touch_int gpio in (FSEL=0, pull=up, IE)
    rmw(0x5000_30A0, 0x7F, 0x70);
    // PA28 -> ws2812_en gpio out (FSEL=0, pull=down)
    rmw(0x5000_30A4, 0x7F, 0x10);
    // PA30 -> i2c3 sda (FSEL=4, pull=down, IE)
    rmw(0x5000_30AC, 0x7F, 0x54);
    rmw(0x5000_B050, 0x3F00, 0x1E00); // PINR: i2c3 sda = PA30
    // PA31 -> i2c3 scl (FSEL=4, pull=down, IE)
    rmw(0x5000_30B0, 0x7F, 0x54);
    rmw(0x5000_B050, 0x3F, 0x1F); // PINR: i2c3 scl = PA31
    // PA32 -> i2c2 sda (FSEL=4, pull=down, IE)
    rmw(0x5000_30B4, 0x7F, 0x54);
    rmw(0x5000_B04C, 0x3F00, 0x2000); // PINR: i2c2 sda = PA32
    // PA33 -> i2c2 scl (FSEL=4, pull=down, IE)
    rmw(0x5000_30B8, 0x7F, 0x54);
    rmw(0x5000_B04C, 0x3F, 0x21); // PINR: i2c2 scl = PA33
    // PA37 -> spi2 dio (FSEL=2, pull=down)
    rmw(0x5000_30C8, 0x7F, 0x12);
    // PA38 -> spi2 di (FSEL=2, pull=down, IE)
    rmw(0x5000_30CC, 0x7F, 0x52);
    // PA39 -> spi2 clk (FSEL=2, pull=up)
    rmw(0x5000_30D0, 0x7F, 0x32);
    // PA40 -> spi2 cs (FSEL=2, pull=up)
    rmw(0x5000_30D4, 0x7F, 0x32);
    // PA41 -> i2c1 scl (FSEL=4, pull=up, IE)
    rmw(0x5000_30D8, 0x7F, 0x74);
    rmw(0x5000_B048, 0x3F, 0x29); // PINR: i2c1 scl = PA41
    // PA42 -> i2c1 sda (FSEL=4, pull=up, IE)
    rmw(0x5000_30DC, 0x7F, 0x74);
    rmw(0x5000_B048, 0x3F00, 0x2A00); // PINR: i2c1 sda = PA42
    // PA43 -> accel_int gpio in (FSEL=0, pull=down, IE)
    rmw(0x5000_30E0, 0x7F, 0x50);
    // PA44 -> nfc_int gpio in (FSEL=0, pull=down, IE)
    rmw(0x5000_30E4, 0x7F, 0x50);
}

/// USART1 is pre-configured at boot: 1M baud, 8N1, TX=PA19.
/// clk_peri_hpsys is clk_hxt48 (48MHz) independent of system clock.
/// EXR_BUSY must be acquired before writing to avoid conflicts with
/// the hardware debug interface that also owns USART1.
pub fn usart1_write_bytes(bytes: &[u8]) {
    const USART1_BASE: u32 = 0x5008_4000;
    const ISR: *const u32 = (USART1_BASE + 0x1C) as *const u32;
    const TDR: *mut u32 = (USART1_BASE + 0x28) as *mut u32;
    const EXR: *mut u32 = (USART1_BASE + 0x38) as *mut u32;

    const TXE: u32 = 1 << 7; // ISR: TX data register empty
    const TC: u32 = 1 << 6; // ISR: transmission complete
    const EXR_BUSY: u32 = 1 << 0; // EXR: 0 = free, reading 0 auto-sets to 1

    unsafe {
        // Acquire the lock. Reading EXR when BUSY=0 atomically sets it to 1.
        // If the debug interface holds it, spin until it releases.
        while EXR.read_volatile() & EXR_BUSY != 0 {}

        for &b in bytes {
            while ISR.read_volatile() & TXE == 0 {}
            TDR.write_volatile(b as u32);
        }

        // Wait for shift register to drain before releasing the lock.
        while ISR.read_volatile() & TC == 0 {}

        // Release the lock.
        EXR.write_volatile(EXR_BUSY);
    }
}

pub fn usart1_write_str(s: &str) {
    usart1_write_bytes(s.as_bytes());
}

fn usart1_write_u32_hex(label: &str, val: u32) {
    usart1_write_str(label);
    let nibbles = [
        (val >> 28) & 0xF,
        (val >> 24) & 0xF,
        (val >> 20) & 0xF,
        (val >> 16) & 0xF,
        (val >> 12) & 0xF,
        (val >> 8) & 0xF,
        (val >> 4) & 0xF,
        val & 0xF,
    ];
    let mut hex = [0u8; 8];
    for (i, &n) in nibbles.iter().enumerate() {
        hex[i] = if n < 10 {
            b'0' + n as u8
        } else {
            b'a' + n as u8 - 10
        };
    }
    usart1_write_bytes(&hex);
    usart1_write_str("\r\n");
}

const TICK_ZERO: &str = "T0000000000000000 ";

fn clock_setup() -> u32 {
    use sifli_pac::hpsys_rcc::vals::SelSys;

    let rcc = sifli_pac::HPSYS_RCC;
    let cfg = sifli_pac::HPSYS_CFG;
    let aon = sifli_pac::HPSYS_AON;
    let pmuc = sifli_pac::PMUC;

    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: clock: start\r\n");

    // 1. Bandgap + PSW
    cfg.cau2_cr().modify(|w| {
        w.set_hpbg_en(true);
        w.set_hpbg_vddpsw_en(true);
    });
    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: clock: bandgap enabled\r\n");

    // 2. DVFS S1 voltages (≤240MHz: ldo=0xD, buck=0xF)
    pmuc.buck_vout().modify(|w| w.set_vout(0xF));
    pmuc.hpsys_vout().modify(|w| w.set_vout(0xD));
    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: clock: voltages set\r\n");

    // 3. Switch to S-mode LDO, wait 250µs for buck to settle
    cfg.syscr().modify(|w| w.set_ldo_vsel(false));
    cortex_m::asm::delay(12_000); // 250µs @ 48MHz
    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: clock: LDO -> S-mode\r\n");

    // 4. Start HXT48 crystal, wait for stable
    aon.acr().modify(|w| w.set_hxt48_req(true));
    while !aon.acr().read().hxt48_rdy() {}
    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: clock: HXT48 ready\r\n");

    // 5. Enable HXT48 -> DLL buffer.
    //    Without this the DLL has no reference and locks to nothing.
    pmuc.hxt_cr1().modify(|w| w.set_buf_dll_en(true));
    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: clock: DLL buffer enabled\r\n");

    // 6. RAM/ROM timing for 240MHz — must be before the clock switch
    cfg.ulpmcr().modify(|w| {
        w.set_ram_rm(3);
        w.set_ram_rme(true);
        w.set_ram_ra(0);
        w.set_ram_wa(4);
        w.set_ram_wpulse(0);
        w.set_rom_rm(3);
        w.set_rom_rme(true);
    });
    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: clock: ULPMCR set\r\n");

    // 7. Configure DLL1 for 240MHz and wait for lock.
    //    in_div2_en: ref = HXT48/2 = 24MHz; stg=9 → (9+1)*24 = 240MHz
    rcc.dllcr(0).modify(|w| w.set_en(false));
    rcc.dllcr(0).modify(|w| {
        w.set_stg(9);
        w.set_in_div2_en(true);
        w.set_out_div2_en(false);
        w.set_en(true);
    });
    while !rcc.dllcr(0).read().ready() {}
    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: clock: DLL1 locked\r\n");

    // 8. Set dividers before switching — keeps APB within spec at 240MHz.
    //    hclk = sys/1 = 240MHz, pclk1 = hclk>>1 = 120MHz, pclk2 = hclk>>6 = 3.75MHz
    rcc.cfgr().modify(|w| {
        w.set_hdiv(1);
        w.set_pdiv1(1); // >>1 = /2
        w.set_pdiv2(6); // >>6 = /64
    });
    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: clock: dividers set\r\n");

    // 9. Switch system clock to DLL1
    rcc.csr().modify(|w| w.set_sel_sys(SelSys::Dll1));
    cortex_m::asm::dsb();
    cortex_m::asm::isb();

    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: clock: finalized\r\n");

    240_000
}

#[cortex_m_rt::exception]
unsafe fn HardFault(frame: &cortex_m_rt::ExceptionFrame) -> ! {
    usart1_write_str("kernel: HARDFAULT\r\n");
    usart1_write_u32_hex("kernel:   PC=0x", frame.pc());
    usart1_write_u32_hex("kernel:   LR=0x", frame.lr());
    usart1_write_u32_hex("kernel:   R0=0x", frame.r0());
    usart1_write_u32_hex("kernel:   R1=0x", frame.r1());
    usart1_write_u32_hex("kernel:   R2=0x", frame.r2());
    usart1_write_u32_hex("kernel:   R3=0x", frame.r3());
    usart1_write_u32_hex("kernel:   R12=0x", frame.r12());
    usart1_write_u32_hex("kernel:   xPSR=0x", frame.xpsr());

    // Fault status registers
    const CFSR: *const u32 = 0xE000_ED28 as *const u32;
    const MMFAR: *const u32 = 0xE000_ED34 as *const u32;
    const BFAR: *const u32 = 0xE000_ED38 as *const u32;
    usart1_write_u32_hex("kernel:   CFSR=0x", core::ptr::read_volatile(CFSR));
    usart1_write_u32_hex("kernel:   MMFAR=0x", core::ptr::read_volatile(MMFAR));
    usart1_write_u32_hex("kernel:   BFAR=0x", core::ptr::read_volatile(BFAR));

    #[allow(clippy::empty_loop)]
    loop {}
}

const VECTOR_TABLE_OFFSET_REGISTER: *mut u32 = 0xE000_ED08 as *mut u32;

#[entry]
fn main() -> ! {
    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: Awake - ハロー世界\r\n");

    // Point the CPU at our vector table (VTOR = SCB + 0x08)
    unsafe {
        // __reset_vector is at .vector_table + 0x08; subtract 8 to get the base
        core::ptr::write_volatile(
            VECTOR_TABLE_OFFSET_REGISTER,
            (&__reset_vector as *const u32 as u32) - 8,
        );
    }

    usart1_write_str(TICK_ZERO);
    usart1_write_u32_hex("kernel: Set VTOR to 0x", unsafe {
        core::ptr::read_volatile(VECTOR_TABLE_OFFSET_REGISTER)
    });

    unsafe { apply_pin_config() };

    let cycles_per_ms = clock_setup();

    // kernel time!
    usart1_write_str(TICK_ZERO);
    usart1_write_str("kernel: starting hubris\r\n");
    unsafe { hubris_kern::startup::start_kernel(cycles_per_ms) }
}

struct UsartWriter;

impl core::fmt::Write for UsartWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        usart1_write_str(s);
        Ok(())
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::fmt::Write;
    let mut w = UsartWriter;

    let _ = write!(w, "kernel: PANIC: {}\r\n", info.message());
    if let Some(loc) = info.location() {
        let _ = write!(w, "  at {}:{}\r\n", loc.file(), loc.line());
    }

    #[allow(clippy::empty_loop)]
    loop {
        core::hint::spin_loop();
    }
}
