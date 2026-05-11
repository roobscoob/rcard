#![no_std]
#![no_main]

use core::sync::atomic::{compiler_fence, Ordering};

use sysmodule_clocks_api::*;

const DLL_POLL_LIMIT: u32 = 100_000;

struct ClocksImpl;

impl Clocks for ClocksImpl {
    fn enable(_meta: ipc::Meta, peripheral: Peripheral) -> Result<(), ClocksError> {
        let rcc = sifli_pac::HPSYS_RCC;
        match peripheral {
            Peripheral::Lcdc1 => rcc.enr1().modify(|w| w.set_lcdc1(true)),
            Peripheral::Mpi1 => rcc.enr2().modify(|w| w.set_mpi1(true)),
            Peripheral::Mpi2 => rcc.enr2().modify(|w| w.set_mpi2(true)),
            Peripheral::Usbc => rcc.enr2().modify(|w| w.set_usbc(true)),
            Peripheral::Trng => rcc.enr1().modify(|w| w.set_trng(true)),
            Peripheral::I2c2 => rcc.enr1().modify(|w| w.set_i2c2(true)),
            Peripheral::I2c3 => rcc.enr2().modify(|w| w.set_i2c3(true)),
            Peripheral::Gptim2 => rcc.enr1().modify(|w| w.set_gptim2(true)),
        }
        Ok(())
    }

    fn disable(_meta: ipc::Meta, peripheral: Peripheral) -> Result<(), ClocksError> {
        let rcc = sifli_pac::HPSYS_RCC;
        match peripheral {
            Peripheral::Lcdc1 => rcc.enr1().modify(|w| w.set_lcdc1(false)),
            Peripheral::Mpi1 => rcc.enr2().modify(|w| w.set_mpi1(false)),
            Peripheral::Mpi2 => rcc.enr2().modify(|w| w.set_mpi2(false)),
            Peripheral::Usbc => rcc.enr2().modify(|w| w.set_usbc(false)),
            Peripheral::Trng => rcc.enr1().modify(|w| w.set_trng(false)),
            Peripheral::I2c2 => rcc.enr1().modify(|w| w.set_i2c2(false)),
            Peripheral::I2c3 => rcc.enr2().modify(|w| w.set_i2c3(false)),
            Peripheral::Gptim2 => rcc.enr1().modify(|w| w.set_gptim2(false)),
        }
        Ok(())
    }

    fn reset(_meta: ipc::Meta, peripheral: Peripheral) -> Result<(), ClocksError> {
        let rcc = sifli_pac::HPSYS_RCC;
        match peripheral {
            Peripheral::Lcdc1 => {
                rcc.rstr1().modify(|w| w.set_lcdc1(true));
                compiler_fence(Ordering::SeqCst);
                rcc.rstr1().modify(|w| w.set_lcdc1(false));
            }
            Peripheral::Mpi1 => {
                rcc.rstr2().modify(|w| w.set_mpi1(true));
                compiler_fence(Ordering::SeqCst);
                rcc.rstr2().modify(|w| w.set_mpi1(false));
            }
            Peripheral::Mpi2 => {
                rcc.rstr2().modify(|w| w.set_mpi2(true));
                compiler_fence(Ordering::SeqCst);
                rcc.rstr2().modify(|w| w.set_mpi2(false));
            }
            Peripheral::Usbc => {
                rcc.rstr2().modify(|w| w.set_usbc(true));
                compiler_fence(Ordering::SeqCst);
                rcc.rstr2().modify(|w| w.set_usbc(false));
            }
            Peripheral::Trng => {
                rcc.rstr1().modify(|w| w.set_trng(true));
                compiler_fence(Ordering::SeqCst);
                rcc.rstr1().modify(|w| w.set_trng(false));
            }
            Peripheral::I2c2 => {
                rcc.rstr1().modify(|w| w.set_i2c2(true));
                compiler_fence(Ordering::SeqCst);
                rcc.rstr1().modify(|w| w.set_i2c2(false));
            }
            Peripheral::I2c3 => {
                rcc.rstr2().modify(|w| w.set_i2c3(true));
                compiler_fence(Ordering::SeqCst);
                rcc.rstr2().modify(|w| w.set_i2c3(false));
            }
            Peripheral::Gptim2 => {
                rcc.rstr1().modify(|w| w.set_gptim2(true));
                compiler_fence(Ordering::SeqCst);
                rcc.rstr1().modify(|w| w.set_gptim2(false));
            }
        }
        Ok(())
    }

    fn configure_dll(_meta: ipc::Meta, dll: DllIndex, config: DllConfig) -> Result<(), ClocksError> {
        let rcc = sifli_pac::HPSYS_RCC;
        let index = match dll {
            DllIndex::Dll2 => 1,
        };

        rcc.dllcr(index).modify(|w| w.set_en(false));
        rcc.dllcr(index).modify(|w| {
            w.set_in_div2_en(config.in_div2_en);
            w.set_out_div2_en(config.out_div2_en);
            w.set_stg(config.stg);
            w.set_en(true);
        });

        let mut polls = 0u32;
        while !rcc.dllcr(index).read().ready() {
            polls += 1;
            if polls >= DLL_POLL_LIMIT {
                return Err(ClocksError::DllLockTimeout);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }

    fn set_clock_source(
        _meta: ipc::Meta,
        peripheral: Peripheral,
        source: ClockSource,
    ) -> Result<(), ClocksError> {
        let rcc = sifli_pac::HPSYS_RCC;
        let val = match source {
            ClockSource::Dll2 => 2,
        };
        match peripheral {
            Peripheral::Mpi1 => rcc.csr().modify(|w| w.set_sel_mpi1(val)),
            _ => return Err(ClocksError::InvalidPeripheral),
        }
        Ok(())
    }

    fn set_divider(_meta: ipc::Meta, peripheral: Peripheral, divider: u8) -> Result<(), ClocksError> {
        let rcc = sifli_pac::HPSYS_RCC;
        match peripheral {
            Peripheral::Usbc => rcc.usbcr().modify(|w| w.set_div(divider)),
            _ => return Err(ClocksError::InvalidPeripheral),
        }
        Ok(())
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    userlib::sys_panic(b"clocks panic")
}

#[export_name = "main"]
fn main() -> ! {
    ipc::server! {
        Clocks: ClocksImpl,
    }
}
