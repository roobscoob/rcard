#![no_std]
#![no_main]

use generated::slots::SLOTS;
use rcard_log::{error, info, trace};
use sysmodule_cap1208_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);
sysmodule_clocks_api::bind_clocks!(Clocks = SLOTS.sysmodule_clocks);

// Both chips are CAP1208-1 (address 0x28), on separate I2C buses.
// Device::A (top surface) = I2C2 (PA32 SDA, PA33 SCL)
// Device::B (bottom surface) = I2C3 (PA30 SDA, PA31 SCL)
const I2C_ADDR: u8 = 0x28;

// CAP1208 register addresses
const REG_MAIN_CONTROL: u8 = 0x00;
const REG_SENSOR_DELTA_BASE: u8 = 0x10;
const REG_SENSITIVITY_CONTROL: u8 = 0x1F;
const REG_CONFIG: u8 = 0x20;
const REG_SENSOR_INPUT_ENABLE: u8 = 0x21;
const REG_SENSOR_INPUT_CONFIG: u8 = 0x22;
const REG_AVG_SAMPLING_CONFIG: u8 = 0x24;
const REG_CALIBRATION_ACTIVATE: u8 = 0x26;
const REG_INTERRUPT_ENABLE: u8 = 0x27;
const REG_REPEAT_RATE_ENABLE: u8 = 0x28;
const REG_MULTI_TOUCH_CONFIG: u8 = 0x2A;
const REG_RECAL_CONFIG: u8 = 0x2F;
const REG_CONFIG2: u8 = 0x44;
const REG_SENSOR_BASE_COUNT_BASE: u8 = 0x50;
const REG_PRODUCT_ID: u8 = 0xFD;
const REG_MANUFACTURER_ID: u8 = 0xFE;
const REG_REVISION: u8 = 0xFF;

const EXPECTED_PRODUCT_ID: u8 = 0x6B;

const I2C_TIMEOUT_MS: u64 = 50;

fn ticks_now() -> u64 {
    userlib::sys_get_timer().now
}

fn i2c_for(device: Device) -> sifli_pac::i2c::I2c {
    match device {
        Device::A => sifli_pac::I2C2,
        Device::B => sifli_pac::I2C3,
    }
}

fn init_i2c_bus(i2c: sifli_pac::i2c::I2c) {
    // Replicate the SiFli HAL_I2C_Init sequence exactly:
    // fclk = 48 MHz, ClockSpeed = 400 kHz
    // flv = ((48M + 200k) / 400k - 0 - 7 + 1) / 2 = 57
    // slv = 0x1FF (max, for bus-reset only)
    // cnt = (57 / 2) - 3 = 25

    // Pulse UR (unit reset) to clear the controller's internal state
    // machine — HAL_I2C_Init relies on HAL_RCC_ResetModule for this,
    // and drv_i2c.c has an explicit UR workaround for "I2C Wait TE fail."
    i2c.cr().write(|w| w.set_ur(true));
    for _ in 0..50u32 {
        core::hint::spin_loop();
    }
    i2c.cr().write(|w| {
        w.set_mode(0x01); // fast mode
        w.set_scle(true);
    });
    i2c.lcr().write(|w| {
        w.set_flv(57);
        w.set_slv(0x1FF);
    });
    i2c.ier().write(|_| {});
    i2c.wcr().write(|w| w.set_cnt(25));
}

fn i2c_enable(i2c: sifli_pac::i2c::I2c) {
    i2c.cr().modify(|w| w.set_iue(true));
}

fn i2c_disable(i2c: sifli_pac::i2c::I2c) {
    i2c.cr().modify(|w| w.set_iue(false));
}

fn wait_te(i2c: sifli_pac::i2c::I2c) -> bool {
    let deadline = ticks_now() + I2C_TIMEOUT_MS;
    loop {
        let sr = i2c.sr().read();
        if sr.te() {
            i2c.sr().write(|w| w.set_te(true));
            return true;
        }
        if sr.bed() {
            i2c.sr().write(|w| w.set_bed(true));
            error!("i2c bus error during transmit");
            return false;
        }
        if ticks_now() >= deadline {
            let tcr = i2c.tcr().read().0;
            let cr = i2c.cr().read().0;
            let bmr = i2c.bmr().read();
            error!(
                "i2c TE timeout: sr={} cr={} tcr={} scl={} sda={}",
                sr.0,
                cr,
                tcr,
                bmr.scl() as u8,
                bmr.sda() as u8,
            );
            return false;
        }
    }
}

fn wait_rf(i2c: sifli_pac::i2c::I2c) -> bool {
    let deadline = ticks_now() + I2C_TIMEOUT_MS;
    loop {
        let sr = i2c.sr().read();
        if sr.rf() {
            i2c.sr().write(|w| w.set_rf(true));
            return true;
        }
        if sr.bed() {
            i2c.sr().write(|w| w.set_bed(true));
            error!("i2c bus error during receive");
            return false;
        }
        if ticks_now() >= deadline {
            let bmr = i2c.bmr().read();
            error!(
                "i2c RF timeout: sr={} scl={} sda={}",
                sr.0,
                bmr.scl() as u8,
                bmr.sda() as u8,
            );
            return false;
        }
    }
}

fn i2c_write_reg(i2c: sifli_pac::i2c::I2c, reg: u8, val: u8) -> Result<(), Cap1208Error> {
    i2c_enable(i2c);

    i2c.dbr().write(|w| w.set_data(I2C_ADDR << 1));
    i2c.tcr().write(|w| {
        w.set_start(true);
        w.set_tb(true);
    });
    if !wait_te(i2c) {
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cTimeout);
    }
    if i2c.sr().read().nack() {
        error!("i2c nack on address byte (write reg 0x{:02x})", reg);
        i2c.tcr().write(|w| w.set_ma(true));
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cNack);
    }

    i2c.dbr().write(|w| w.set_data(reg));
    i2c.tcr().write(|w| w.set_tb(true));
    if !wait_te(i2c) {
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cTimeout);
    }
    if i2c.sr().read().nack() {
        error!("i2c nack on register address 0x{:02x}", reg);
        i2c.tcr().write(|w| w.set_ma(true));
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cNack);
    }

    i2c.dbr().write(|w| w.set_data(val));
    i2c.tcr().write(|w| {
        w.set_stop(true);
        w.set_tb(true);
    });
    if !wait_te(i2c) {
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cTimeout);
    }

    i2c_disable(i2c);
    Ok(())
}

fn i2c_read_reg(i2c: sifli_pac::i2c::I2c, reg: u8) -> Result<u8, Cap1208Error> {
    i2c_enable(i2c);

    i2c.dbr().write(|w| w.set_data(I2C_ADDR << 1));
    i2c.tcr().write(|w| {
        w.set_start(true);
        w.set_tb(true);
    });
    if !wait_te(i2c) {
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cTimeout);
    }
    if i2c.sr().read().nack() {
        error!("i2c nack on address byte (read reg 0x{:02x})", reg);
        i2c.tcr().write(|w| w.set_ma(true));
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cNack);
    }

    i2c.dbr().write(|w| w.set_data(reg));
    i2c.tcr().write(|w| w.set_tb(true));
    if !wait_te(i2c) {
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cTimeout);
    }
    if i2c.sr().read().nack() {
        error!("i2c nack on register address 0x{:02x}", reg);
        i2c.tcr().write(|w| w.set_ma(true));
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cNack);
    }

    i2c.dbr().write(|w| w.set_data((I2C_ADDR << 1) | 1));
    i2c.tcr().write(|w| {
        w.set_start(true);
        w.set_tb(true);
    });
    if !wait_te(i2c) {
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cTimeout);
    }
    if i2c.sr().read().nack() {
        error!("i2c nack on repeated start (read reg 0x{:02x})", reg);
        i2c.tcr().write(|w| w.set_ma(true));
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cNack);
    }

    i2c.tcr().write(|w| {
        w.set_nack(true);
        w.set_stop(true);
        w.set_tb(true);
    });
    if !wait_rf(i2c) {
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cTimeout);
    }

    let val = i2c.dbr().read().data();
    i2c_disable(i2c);
    Ok(val)
}

fn i2c_read_block(
    i2c: sifli_pac::i2c::I2c,
    start_reg: u8,
    buf: &mut [u8],
) -> Result<(), Cap1208Error> {
    if buf.is_empty() {
        return Ok(());
    }

    i2c_enable(i2c);

    i2c.dbr().write(|w| w.set_data(I2C_ADDR << 1));
    i2c.tcr().write(|w| {
        w.set_start(true);
        w.set_tb(true);
    });
    if !wait_te(i2c) {
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cTimeout);
    }
    if i2c.sr().read().nack() {
        error!("i2c nack on address byte (block read 0x{:02x})", start_reg);
        i2c.tcr().write(|w| w.set_ma(true));
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cNack);
    }

    i2c.dbr().write(|w| w.set_data(start_reg));
    i2c.tcr().write(|w| w.set_tb(true));
    if !wait_te(i2c) {
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cTimeout);
    }
    if i2c.sr().read().nack() {
        error!(
            "i2c nack on register address (block read 0x{:02x})",
            start_reg
        );
        i2c.tcr().write(|w| w.set_ma(true));
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cNack);
    }

    i2c.dbr().write(|w| w.set_data((I2C_ADDR << 1) | 1));
    i2c.tcr().write(|w| {
        w.set_start(true);
        w.set_tb(true);
    });
    if !wait_te(i2c) {
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cTimeout);
    }
    if i2c.sr().read().nack() {
        error!(
            "i2c nack on repeated start (block read 0x{:02x})",
            start_reg
        );
        i2c.tcr().write(|w| w.set_ma(true));
        i2c_disable(i2c);
        return Err(Cap1208Error::I2cNack);
    }

    let last = buf.len() - 1;
    let mut i = 0;
    while i <= last {
        if i == last {
            i2c.tcr().write(|w| {
                w.set_nack(true);
                w.set_stop(true);
                w.set_tb(true);
            });
        } else {
            i2c.tcr().write(|w| w.set_tb(true));
        }
        if !wait_rf(i2c) {
            i2c_disable(i2c);
            return Err(Cap1208Error::I2cTimeout);
        }
        buf[i] = i2c.dbr().read().data();
        i += 1;
    }

    i2c_disable(i2c);
    Ok(())
}

fn configure_chip(i2c: sifli_pac::i2c::I2c, config: &Cap1208Config) -> Result<(), Cap1208Error> {
    let sig = &config.signal;
    let samp = &config.sampling;
    let recal = &config.recalibration;

    i2c_write_reg(i2c, REG_MAIN_CONTROL, 0x00)?;

    // 0x1F: analog_gain [6:4] | digital_shift [3:0]
    let sens_reg = ((sig.analog_gain as u8) << 4) | (sig.digital_shift as u8);
    i2c_write_reg(i2c, REG_SENSITIVITY_CONTROL, sens_reg)?;

    // 0x20: max_dur_en [3] — noise filters left at defaults (enabled)
    let max_dur_en = !matches!(recal.touch_duration, TouchRecalDuration::Disabled);
    i2c_write_reg(i2c, REG_CONFIG, (max_dur_en as u8) << 3)?;

    i2c_write_reg(i2c, REG_SENSOR_INPUT_ENABLE, config.enabled_channels)?;

    // 0x22: max_dur [7:4] — register value is (enum - 1), clamped for Disabled
    let max_dur_bits = if max_dur_en {
        (recal.touch_duration as u8) - 1
    } else {
        0
    };
    i2c_write_reg(i2c, REG_SENSOR_INPUT_CONFIG, max_dur_bits << 4)?;

    // 0x24: avg [6:4] | duration [3:2] | cycle_time [1:0]
    let avg_reg = ((samp.averaging as u8) << 4)
        | ((samp.duration as u8) << 2)
        | (samp.cycle_time as u8);
    i2c_write_reg(i2c, REG_AVG_SAMPLING_CONFIG, avg_reg)?;

    i2c_write_reg(i2c, REG_INTERRUPT_ENABLE, 0x00)?;
    i2c_write_reg(i2c, REG_REPEAT_RATE_ENABLE, 0x00)?;
    i2c_write_reg(i2c, REG_MULTI_TOUCH_CONFIG, 0x00)?;

    // 0x2F: neg_delta_cnt [4:3] | cal_cfg [2:0]
    let recal_reg = ((recal.below_baseline as u8) << 3) | (recal.rate as u8);
    i2c_write_reg(i2c, REG_RECAL_CONFIG, recal_reg)?;

    // 0x44: leave at defaults (noise filters on, power ctrl off)
    i2c_write_reg(i2c, REG_CONFIG2, 0x40)?;

    Ok(())
}

fn compute_gain_divisor(gain: AnalogGain) -> i32 {
    match gain {
        AnalogGain::X128 => 1,
        AnalogGain::X64 => 2,
        AnalogGain::X32 => 4,
        AnalogGain::X16 => 8,
        AnalogGain::X8 => 16,
        AnalogGain::X4 => 32,
        AnalogGain::X2 => 64,
        AnalogGain::X1 => 128,
    }
}

struct Cap1208Resource {
    device: Device,
    analog_gain: AnalogGain,
    digital_shift: DigitalShift,
}

static mut SLOT_USED: [bool; 2] = [false; 2];

impl Cap1208 for Cap1208Resource {
    fn open(meta: ipc::Meta, device: Device, config: Cap1208Config) -> Result<Self, Cap1208Error> {
        let _ = meta;
        let idx = device as usize;

        if unsafe { SLOT_USED[idx] } {
            error!("device {:?} already open", device);
            return Err(Cap1208Error::AlreadyOpen);
        }

        let i2c = i2c_for(device);

        info!("opening device {:?}, configuring chip", device);
        configure_chip(i2c, &config)?;

        let product_id = i2c_read_reg(i2c, REG_PRODUCT_ID)?;
        if product_id != EXPECTED_PRODUCT_ID {
            error!(
                "device {:?}: unexpected product id 0x{:02x} (expected 0x{:02x})",
                device, product_id, EXPECTED_PRODUCT_ID
            );
            return Err(Cap1208Error::UnexpectedProductId);
        }

        let manufacturer_id = i2c_read_reg(i2c, REG_MANUFACTURER_ID)?;
        let revision = i2c_read_reg(i2c, REG_REVISION)?;
        info!(
            "device {:?}: product=0x{:02x} mfr=0x{:02x} rev=0x{:02x}",
            device, product_id, manufacturer_id, revision
        );

        unsafe { SLOT_USED[idx] = true };

        Ok(Cap1208Resource {
            device,
            analog_gain: config.signal.analog_gain,
            digital_shift: config.signal.digital_shift,
        })
    }

    fn read(&mut self, _meta: ipc::Meta) -> Result<[i32; 8], Cap1208Error> {
        let i2c = i2c_for(self.device);

        let main_ctrl = i2c_read_reg(i2c, REG_MAIN_CONTROL)?;
        if main_ctrl & 0x01 != 0 {
            i2c_write_reg(i2c, REG_MAIN_CONTROL, main_ctrl & !0x01)?;
        }

        let mut delta_raw = [0u8; 8];
        i2c_read_block(i2c, REG_SENSOR_DELTA_BASE, &mut delta_raw)?;

        let mut base_raw = [0u8; 8];
        i2c_read_block(i2c, REG_SENSOR_BASE_COUNT_BASE, &mut base_raw)?;

        let base_scale = 1i32 << (self.digital_shift as u32);
        let delta_scale = compute_gain_divisor(self.analog_gain);

        let mut result = [0i32; 8];
        let mut i = 0;
        while i < 8 {
            let base = (base_raw[i] as i32) * base_scale;
            let delta = (delta_raw[i] as i8 as i32) * delta_scale;
            result[i] = base + delta;
            i += 1;
        }

        trace!("device {}: {}", self.device, result);

        Ok(result)
    }

    fn calibrate(&mut self, _meta: ipc::Meta, channels: u8) -> Result<(), Cap1208Error> {
        info!(
            "device {:?}: calibrating channels 0x{:02x}",
            self.device, channels
        );
        i2c_write_reg(i2c_for(self.device), REG_CALIBRATION_ACTIVATE, channels)
    }

    fn read_id(&mut self, _meta: ipc::Meta) -> Result<DeviceId, Cap1208Error> {
        let i2c = i2c_for(self.device);
        let product_id = i2c_read_reg(i2c, REG_PRODUCT_ID)?;
        let manufacturer_id = i2c_read_reg(i2c, REG_MANUFACTURER_ID)?;
        let revision = i2c_read_reg(i2c, REG_REVISION)?;

        Ok(DeviceId {
            product_id,
            manufacturer_id,
            revision,
        })
    }
}

#[export_name = "main"]
fn main() -> ! {
    info!("cap1208: starting");

    if let Err(e) = Clocks::enable(sysmodule_clocks_api::Peripheral::I2c2) {
        error!("failed to enable I2C2 clock: {:?}", e);
    }
    if let Err(e) = Clocks::enable(sysmodule_clocks_api::Peripheral::I2c3) {
        error!("failed to enable I2C3 clock: {:?}", e);
    }

    // HAL_I2C_Init always calls HAL_RCC_ResetModule before configuring.
    // Reset modules to bring all registers to a known-good state.
    if let Err(e) = Clocks::reset(sysmodule_clocks_api::Peripheral::I2c2) {
        error!("failed to reset I2C2: {:?}", e);
    }
    if let Err(e) = Clocks::reset(sysmodule_clocks_api::Peripheral::I2c3) {
        error!("failed to reset I2C3: {:?}", e);
    }

    // Re-write PINR after clock enable — the output routing may latch
    // on clock gate open and miss the kernel's boot-time configuration.
    let cfg = sifli_pac::HPSYS_CFG;
    cfg.i2c2_pinr().write(|w| {
        w.set_scl_pin(33); // PA33
        w.set_sda_pin(32); // PA32
    });
    cfg.i2c3_pinr().write(|w| {
        w.set_scl_pin(31); // PA31
        w.set_sda_pin(30); // PA30
    });

    init_i2c_bus(sifli_pac::I2C2);
    init_i2c_bus(sifli_pac::I2C3);

    for (name, i2c) in [("I2C2", sifli_pac::I2C2), ("I2C3", sifli_pac::I2C3)] {
        info!(
            "{}: cr={} lcr={} wcr={} sr={}",
            name,
            i2c.cr().read().0,
            i2c.lcr().read().0,
            i2c.wcr().read().0,
            i2c.sr().read().0,
        );
    }

    // One-shot bus probe: enable I2C2, fire one byte, sleep 100ms, read back.
    {
        let i2c = sifli_pac::I2C2;
        i2c.cr().modify(|w| w.set_iue(true));
        i2c.dbr().write(|w| w.set_data(I2C_ADDR << 1));
        i2c.tcr().write(|w| {
            w.set_start(true);
            w.set_tb(true);
        });
        let deadline = ticks_now() + 100;
        while ticks_now() < deadline {}
        let sr = i2c.sr().read();
        let bmr = i2c.bmr().read();
        info!(
            "probe after 100ms: sr={} cr={} tcr={} scl={} sda={}",
            sr.0,
            i2c.cr().read().0,
            i2c.tcr().read().0,
            bmr.scl() as u8,
            bmr.sda() as u8,
        );
        // Clean up: abort + disable
        i2c.tcr().write(|w| w.set_ma(true));
        i2c.cr().modify(|w| w.set_iue(false));
    }

    info!("cap1208: i2c buses initialized, entering server loop");

    ipc::server! {
        Cap1208: Cap1208Resource,
    }
}
