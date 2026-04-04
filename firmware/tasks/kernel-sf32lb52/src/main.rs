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

include!(concat!(env!("OUT_DIR"), "/pin_config.rs"));

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

// fn usart1_write_u32_hex(label: &str, val: u32) {
//     usart1_write_str(label);
//     let nibbles = [
//         (val >> 28) & 0xF,
//         (val >> 24) & 0xF,
//         (val >> 20) & 0xF,
//         (val >> 16) & 0xF,
//         (val >> 12) & 0xF,
//         (val >> 8) & 0xF,
//         (val >> 4) & 0xF,
//         val & 0xF,
//     ];
//     let mut hex = [0u8; 8];
//     for (i, &n) in nibbles.iter().enumerate() {
//         hex[i] = if n < 10 {
//             b'0' + n as u8
//         } else {
//             b'a' + n as u8 - 10
//         };
//     }
//     usart1_write_bytes(&hex);
//     usart1_write_str("\r\n");
// }

fn clock_setup() -> u32 {
    use sifli_pac::hpsys_rcc::vals::SelSys;

    let rcc = sifli_pac::HPSYS_RCC;
    let cfg = sifli_pac::HPSYS_CFG;
    let aon = sifli_pac::HPSYS_AON;
    let pmuc = sifli_pac::PMUC;

    usart1_write_str("clock_setup: start\r\n");

    // 1. Bandgap + PSW
    cfg.cau2_cr().modify(|w| {
        w.set_hpbg_en(true);
        w.set_hpbg_vddpsw_en(true);
    });
    usart1_write_str("clock_setup: bandgap enabled\r\n");

    // 2. DVFS S1 voltages (≤240MHz: ldo=0xD, buck=0xF)
    pmuc.buck_vout().modify(|w| w.set_vout(0xF));
    pmuc.hpsys_vout().modify(|w| w.set_vout(0xD));
    usart1_write_str("clock_setup: voltages set\r\n");

    // 3. Switch to S-mode LDO, wait 250µs for buck to settle
    cfg.syscr().modify(|w| w.set_ldo_vsel(false));
    cortex_m::asm::delay(12_000); // 250µs @ 48MHz
    usart1_write_str("clock_setup: LDO -> S-mode\r\n");

    // 4. Start HXT48 crystal, wait for stable
    aon.acr().modify(|w| w.set_hxt48_req(true));
    while !aon.acr().read().hxt48_rdy() {}
    usart1_write_str("clock_setup: HXT48 ready\r\n");

    // 5. Enable HXT48 -> DLL buffer.
    //    Without this the DLL has no reference and locks to nothing.
    pmuc.hxt_cr1().modify(|w| w.set_buf_dll_en(true));
    usart1_write_str("clock_setup: DLL buffer enabled\r\n");

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
    usart1_write_str("clock_setup: ULPMCR set\r\n");

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
    usart1_write_str("clock_setup: DLL1 locked\r\n");

    // 8. Set dividers before switching — keeps APB within spec at 240MHz.
    //    hclk = sys/1 = 240MHz, pclk1 = hclk>>1 = 120MHz, pclk2 = hclk>>6 = 3.75MHz
    rcc.cfgr().modify(|w| {
        w.set_hdiv(1);
        w.set_pdiv1(1); // >>1 = /2
        w.set_pdiv2(6); // >>6 = /64
    });
    usart1_write_str("clock_setup: dividers set\r\n");

    // 9. Switch system clock to DLL1
    rcc.csr().modify(|w| w.set_sel_sys(SelSys::Dll1));
    cortex_m::asm::dsb();
    cortex_m::asm::isb();

    // 10. Recalibrate USART1 for PCLK1 = 120MHz @ 1Mbaud
    //     120MHz / (7 + 8/16) / 16 = 1MHz
    // usart1.brr().write(|w| {
    //     w.set_int(7);
    //     w.set_frac(8);
    // });

    usart1_write_str("clock_setup: finalized\r\n");

    240_000
}

#[cortex_m_rt::exception]
unsafe fn HardFault(_frame: &cortex_m_rt::ExceptionFrame) -> ! {
    usart1_write_str("HARDFAULT\r\n");

    #[allow(clippy::empty_loop)]
    loop {}
}

const VECTOR_TABLE_OFFSET_REGISTER: *mut u32 = 0xE000_ED08 as *mut u32;

#[entry]
fn main() -> ! {
    // Point the CPU at our vector table (VTOR = SCB + 0x08)
    unsafe {
        // __reset_vector is at .vector_table + 0x08; subtract 8 to get the base
        core::ptr::write_volatile(
            VECTOR_TABLE_OFFSET_REGISTER,
            (&__reset_vector as *const u32 as u32) - 8,
        );
    }

    // Apply board pin configuration (generated from app.kdl pin assignments)
    unsafe { apply_pin_config() };

    let cycles_per_ms = clock_setup();

    // kernel time!
    unsafe { hubris_kern::startup::start_kernel(cycles_per_ms) }
}
