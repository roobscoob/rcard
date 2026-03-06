#![no_std]
#![no_main]

use core::mem::MaybeUninit;
use core::ptr::{read_volatile, write_volatile};

use sysmodule_time_api::{SystemDateTime, Time, TimeDispatcher};

const RTC_BASE: usize = 0x500C_B000;
const RTC_TR: *mut u32 = RTC_BASE as *mut u32;           // +0x00
const RTC_DR: *mut u32 = (RTC_BASE + 0x04) as *mut u32;  // +0x04
const RTC_ISR: *mut u32 = (RTC_BASE + 0x0C) as *mut u32; // +0x0C

const ISR_INIT: u32 = 1 << 10;
const ISR_INITF: u32 = 1 << 9;
const ISR_INITS: u32 = 1 << 8;

fn bcd_to_bin(bcd: u8) -> u8 {
    (bcd >> 4) * 10 + (bcd & 0x0F)
}

fn bin_to_bcd(bin: u8) -> u8 {
    ((bin / 10) << 4) | (bin % 10)
}

fn read_time() -> SystemDateTime {
    let tr = unsafe { read_volatile(RTC_TR) };
    let dr = unsafe { read_volatile(RTC_DR) };

    let hour = bcd_to_bin((((tr >> 29) & 0x3) << 4 | ((tr >> 25) & 0xF)) as u8);
    let minute = bcd_to_bin((((tr >> 22) & 0x7) << 4 | ((tr >> 18) & 0xF)) as u8);
    let second = bcd_to_bin((((tr >> 15) & 0x7) << 4 | ((tr >> 11) & 0xF)) as u8);

    let year_bcd = bcd_to_bin((((dr >> 20) & 0xF) << 4 | ((dr >> 16) & 0xF)) as u8);
    let cb = (dr >> 24) & 1 != 0;
    let year = if !cb {
        2000 + year_bcd as u16
    } else {
        2100 + year_bcd as u16
    };
    let month = bcd_to_bin((((dr >> 12) & 0x1) << 4 | ((dr >> 8) & 0xF)) as u8);
    let day = bcd_to_bin((((dr >> 4) & 0x3) << 4 | (dr & 0xF)) as u8);
    let weekday = ((dr >> 13) & 0x7) as u8;

    SystemDateTime {
        year,
        month,
        day,
        weekday,
        hour,
        minute,
        second,
    }
}

fn write_time(dt: &SystemDateTime) {
    let hour_bcd = bin_to_bcd(dt.hour);
    let minute_bcd = bin_to_bcd(dt.minute);
    let second_bcd = bin_to_bcd(dt.second);

    let (year_offset, cb) = if dt.year >= 2100 {
        ((dt.year - 2100) as u8, true)
    } else {
        ((dt.year - 2000) as u8, false)
    };
    let year_bcd = bin_to_bcd(year_offset);
    let month_bcd = bin_to_bcd(dt.month);
    let day_bcd = bin_to_bcd(dt.day);

    let tr_val: u32 =
        ((hour_bcd >> 4) as u32 & 0x3) << 29
        | ((hour_bcd & 0xF) as u32) << 25
        | ((minute_bcd >> 4) as u32 & 0x7) << 22
        | ((minute_bcd & 0xF) as u32) << 18
        | ((second_bcd >> 4) as u32 & 0x7) << 15
        | ((second_bcd & 0xF) as u32) << 11;

    let dr_val: u32 =
        (cb as u32) << 24
        | ((year_bcd >> 4) as u32 & 0xF) << 20
        | ((year_bcd & 0xF) as u32) << 16
        | (dt.weekday as u32 & 0x7) << 13
        | ((month_bcd >> 4) as u32 & 0x1) << 12
        | ((month_bcd & 0xF) as u32) << 8
        | ((day_bcd >> 4) as u32 & 0x3) << 4
        | (day_bcd & 0xF) as u32;

    // Enter init mode
    unsafe {
        let isr = read_volatile(RTC_ISR);
        write_volatile(RTC_ISR, isr | ISR_INIT);
        while read_volatile(RTC_ISR) & ISR_INITF == 0 {}

        write_volatile(RTC_TR, tr_val);
        write_volatile(RTC_DR, dr_val);

        // Exit init mode
        let isr = read_volatile(RTC_ISR);
        write_volatile(RTC_ISR, isr & !ISR_INIT);
    }
}

struct TimeImpl;

fn is_initialized() -> bool {
    unsafe { read_volatile(RTC_ISR) & ISR_INITS != 0 }
}

impl Time for TimeImpl {
    fn get_time(_meta: ipc::Meta) -> Option<SystemDateTime> {
        if is_initialized() {
            Some(read_time())
        } else {
            None
        }
    }

    fn set_time(_meta: ipc::Meta, dt: SystemDateTime) {
        write_time(&dt);
    }
}

#[export_name = "main"]
fn main() -> ! {
    let mut dispatcher = TimeDispatcher::<TimeImpl>::new();
    let mut buf = [MaybeUninit::uninit(); 256];

    ipc::Server::<1>::new()
        .with_dispatcher(0x04, &mut dispatcher)
        .run(&mut buf)
}
