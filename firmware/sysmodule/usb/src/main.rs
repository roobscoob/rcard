#![no_std]
#![no_main]

use core::cell::UnsafeCell;

use generated::slots::SLOTS;
use musb::{MusbInstance, UsbInstance, UsbdBus};
use rcard_log::{debug, info, OptionExt, ResultExt};
use usb_device::device::UsbRev;

#[derive(rcard_log::Format)]
enum UsbdError {
    WouldBlock,
    ParseError,
    BufferOverflow,
    EndpointOverflow,
    EndpointMemoryOverflow,
    InvalidEndpoint,
    Unsupported,
    InvalidState,
}

impl From<usb_device::UsbError> for UsbdError {
    fn from(e: usb_device::UsbError) -> Self {
        match e {
            usb_device::UsbError::WouldBlock => Self::WouldBlock,
            usb_device::UsbError::ParseError => Self::ParseError,
            usb_device::UsbError::BufferOverflow => Self::BufferOverflow,
            usb_device::UsbError::EndpointOverflow => Self::EndpointOverflow,
            usb_device::UsbError::EndpointMemoryOverflow => Self::EndpointMemoryOverflow,
            usb_device::UsbError::InvalidEndpoint => Self::InvalidEndpoint,
            usb_device::UsbError::Unsupported => Self::Unsupported,
            usb_device::UsbError::InvalidState => Self::InvalidState,
        }
    }
}

#[derive(rcard_log::Format)]
enum BuilderErr {
    TooManyLanguages,
    InvalidPacketSize,
    PowerTooHigh,
}

impl From<usb_device::prelude::BuilderError> for BuilderErr {
    fn from(e: usb_device::prelude::BuilderError) -> Self {
        match e {
            usb_device::prelude::BuilderError::TooManyLanguages => Self::TooManyLanguages,
            usb_device::prelude::BuilderError::InvalidPacketSize => Self::InvalidPacketSize,
            usb_device::prelude::BuilderError::PowerTooHigh => Self::PowerTooHigh,
        }
    }
}
use sysmodule_usb_api::*;
use usb_device::bus::UsbBusAllocator;
use usb_device::class::UsbClass;
use usb_device::descriptor::DescriptorWriter;
use usb_device::device::{UsbDevice, UsbDeviceBuilder, UsbDeviceState, UsbVidPid};
use usb_device::endpoint::{EndpointAddress, EndpointIn, EndpointOut};

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);

// ---------------------------------------------------------------------------
// Single-threaded cell (Hubris sysmodules process one IPC message at a time)
// ---------------------------------------------------------------------------

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}
impl<T> SyncCell<T> {
    const fn new(val: T) -> Self {
        Self(UnsafeCell::new(val))
    }
    #[allow(clippy::mut_from_ref)]
    fn get(&self) -> &mut T {
        unsafe { &mut *self.0.get() }
    }
}

// ---------------------------------------------------------------------------
// USB bus allocator (static so usb-device types get 'static lifetime)
// ---------------------------------------------------------------------------

static USB_ALLOC: SyncCell<Option<UsbBusAllocator<UsbdBus<UsbInstance>>>> = SyncCell::new(None);

// ---------------------------------------------------------------------------
// Vendor-specific USB class (declares our endpoints to usb-device)
// ---------------------------------------------------------------------------

struct VendorClass<'a, B: usb_device::bus::UsbBus> {
    /// Interface numbers — one per distinct endpoint group.
    /// Endpoints are assigned to interfaces by endpoint number:
    /// EP1 → ifaces[0] (host-driven), EP2 → ifaces[1] (fob-driven), etc.
    ifaces: [Option<usb_device::bus::InterfaceNumber>; 7],
    ep_in: [Option<EndpointIn<'a, B>>; 7],
    ep_out: [Option<EndpointOut<'a, B>>; 7],
    /// Pre-built MSOS 2.0 platform capability (25 bytes) and descriptor set (30 bytes).
    msos_platform_capability: [u8; 25],
    msos_descriptor_set: [u8; 30],
    msos_vendor_code: u8,
}

impl<'a> VendorClass<'a, UsbdBus<UsbInstance>> {
    fn new(
        alloc: &'a UsbBusAllocator<UsbdBus<UsbInstance>>,
        endpoints_in: &[Option<EndpointConfig>; 7],
        endpoints_out: &[Option<EndpointConfig>; 7],
        identity: &DeviceIdentity,
    ) -> Self {
        let mut class = VendorClass {
            ifaces: [const { None }; 7],
            ep_in: [const { None }; 7],
            ep_out: [const { None }; 7],
            msos_platform_capability: identity.msos_platform_capability,
            msos_descriptor_set: identity.msos_descriptor_set,
            msos_vendor_code: identity.msos_vendor_code,
        };

        for idx in 0..7 {
            let has_in = endpoints_in[idx].is_some();
            let has_out = endpoints_out[idx].is_some();

            if !has_in && !has_out {
                continue;
            }

            let ep_num = (idx + 1) as u8;

            // Allocate one interface per endpoint number.
            // IN + OUT on the same number share an interface.
            if class.ifaces[idx].is_none() {
                class.ifaces[idx] = Some(alloc.interface());
            }

            if let Some(config) = &endpoints_in[idx] {
                let ep_type = transfer_type(config.transfer_type);
                let addr = EndpointAddress::from_parts(
                    ep_num as usize,
                    usb_device::UsbDirection::In,
                );
                class.ep_in[idx] = Some(
                    alloc
                        .alloc(Some(addr), ep_type, config.max_packet_size, config.interval)
                        .map_err(UsbdError::from)
                        .log_expect("EP IN alloc failed"),
                );
            }

            if let Some(config) = &endpoints_out[idx] {
                let ep_type = transfer_type(config.transfer_type);
                let addr = EndpointAddress::from_parts(
                    ep_num as usize,
                    usb_device::UsbDirection::Out,
                );
                class.ep_out[idx] = Some(
                    alloc
                        .alloc(Some(addr), ep_type, config.max_packet_size, config.interval)
                        .map_err(UsbdError::from)
                        .log_expect("EP OUT alloc failed"),
                );
            }
        }

        class
    }
}

fn transfer_type(tt: TransferType) -> usb_device::endpoint::EndpointType {
    match tt {
        TransferType::Bulk => usb_device::endpoint::EndpointType::Bulk,
        TransferType::Interrupt => usb_device::endpoint::EndpointType::Interrupt,
        TransferType::Isochronous => usb_device::endpoint::EndpointType::Isochronous {
            synchronization: usb_device::endpoint::IsochronousSynchronizationType::Asynchronous,
            usage: usb_device::endpoint::IsochronousUsageType::Data,
        },
    }
}

/// Vendor request code for MSOS 2.0 descriptor set retrieval.
impl<B: usb_device::bus::UsbBus> UsbClass<B> for VendorClass<'_, B> {
    fn get_configuration_descriptors(
        &self,
        writer: &mut DescriptorWriter,
    ) -> usb_device::Result<()> {
        // Each endpoint number gets its own interface descriptor.
        // Endpoints sharing a number (IN + OUT pair) are grouped.
        for (idx, iface) in self.ifaces.iter().enumerate() {
            let Some(iface) = iface else { continue };
            writer.interface(*iface, 0xFF, 0x00, 0x00)?;
            if let Some(ep) = &self.ep_out[idx] {
                writer.endpoint(ep)?;
            }
            if let Some(ep) = &self.ep_in[idx] {
                writer.endpoint(ep)?;
            }
        }
        Ok(())
    }

    fn get_bos_descriptors(
        &self,
        writer: &mut usb_device::descriptor::BosWriter,
    ) -> usb_device::Result<()> {
        if self.msos_platform_capability[1] != 0 {
            // Platform capability type = 0x05
            writer.capability(0x05, &self.msos_platform_capability)?;
        }
        Ok(())
    }

    fn control_in(&mut self, xfer: usb_device::class::ControlIn<B>) {
        let req = xfer.request();
        if self.msos_vendor_code != 0
            && req.request_type == usb_device::control::RequestType::Vendor
            && req.request == self.msos_vendor_code
            && req.index == 0x07
        {
            let _ = xfer.accept_with(&self.msos_descriptor_set);
        }
    }
}

// ---------------------------------------------------------------------------
// Global sysmodule state
// ---------------------------------------------------------------------------

struct UsbGlobal {
    identity: DeviceIdentity,
    endpoint_count: u8,

    // Endpoint tracking (index 0 = EP1, index 6 = EP7).
    // Split by direction — USB hardware has separate TX/RX FIFOs per
    // endpoint number, so EP1 IN and EP1 OUT are independent.
    endpoints_in: [Option<EndpointConfig>; 7],
    endpoints_out: [Option<EndpointConfig>; 7],

    // usb-device state (populated when all endpoints are opened)
    device: Option<UsbDevice<'static, UsbdBus<UsbInstance>>>,
    class: Option<VendorClass<'static, UsbdBus<UsbInstance>>>,

    // Ownership tracking
    bus_taken: bool,
    handles_issued: u8,
    handles_consumed: u8, // bitmask
    endpoints_opened: u8,
}

impl UsbGlobal {
    const fn new() -> Self {
        Self {
            identity: DeviceIdentity {
                vendor_id: 0,
                product_id: 0,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                bcd_device: 0,
                manufacturer: [0; 32],
                product: [0; 32],
                serial: [0; 32],
                msos_platform_capability: [0; 25],
                msos_descriptor_set: [0; 30],
                msos_vendor_code: 0,
            },
            endpoint_count: 0,
            endpoints_in: [None; 7],
            endpoints_out: [None; 7],
            device: None,
            class: None,
            bus_taken: false,
            handles_issued: 0,
            handles_consumed: 0,
            endpoints_opened: 0,
        }
    }

    /// Build the usb-device stack and attach to the bus.
    /// Called when the last endpoint is opened.
    fn activate(&mut self) {
        // First: clear any stale MUSB state with a peripheral reset
        sifli_pac::HPSYS_RCC.rstr2().modify(|w| w.set_usbc(true));
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        // brief spin
        for _ in 0..100 {
            core::hint::spin_loop();
        }
        sifli_pac::HPSYS_RCC.rstr2().modify(|w| w.set_usbc(false));
        // Re-enable clock after reset
        sifli_pac::HPSYS_RCC.enr2().modify(|w| w.set_usbc(true));
        let alloc = USB_ALLOC.get().as_ref().log_expect("USB allocator missing");

        let power_before = UsbInstance::regs().power().read();
        debug!("MUSB power before activate: {}", power_before.0);

        // Create our class (allocates endpoints from usb-device)
        let class = VendorClass::new(alloc, &self.endpoints_in, &self.endpoints_out, &self.identity);

        // SAFETY: UsbGlobal only exists inside `static USB`, so self.identity
        // has 'static lifetime. We reborrow through a pointer to decouple
        // from &mut self, which we need for self.poll() below.
        // note from rose hall: i hate this.
        //                      claude pressured me into adding it, and honestly i don't see a better solution since it really wants a pointer
        //                      but it still makes me queasy. if you have suggestions, please let me know.
        #[allow(clippy::deref_addrof)]
        let id: &'static DeviceIdentity = unsafe { &*(core::ptr::addr_of!(self.identity)) };

        let mut strings = usb_device::device::StringDescriptors::default();
        let mfr = id.manufacturer_str();
        if !mfr.is_empty() {
            strings = strings.manufacturer(mfr);
        }
        let prod = id.product_str();
        if !prod.is_empty() {
            strings = strings.product(prod);
        }
        let ser = id.serial_str();
        if !ser.is_empty() {
            strings = strings.serial_number(ser);
        }

        let device = UsbDeviceBuilder::new(alloc, UsbVidPid(id.vendor_id, id.product_id))
            .usb_rev(if id.has_msos() {
                UsbRev::Usb210
            } else {
                UsbRev::Usb200
            })
            .strings(&[strings])
            .map_err(BuilderErr::from)
            .log_expect("string descriptors failed")
            .device_class(id.device_class)
            .device_sub_class(id.device_subclass)
            .device_protocol(id.device_protocol)
            .device_release(id.bcd_device)
            .self_powered(false)
            .max_packet_size_0(64)
            .map_err(BuilderErr::from)
            .log_expect("EP0 alloc failed")
            .build();

        self.class = Some(class);
        self.device = Some(device);

        // Enable MUSB interrupt status registers (required for on_interrupt
        // to see bus events — intrusb/intrtx/intrrx are gated by these)
        musb::common_impl::bus_init::<UsbInstance>();

        // Run the first poll to trigger usb-device's bus.enable(),
        // THEN attach — otherwise the host sees us before EP0 is ready
        self.poll();

        // Attach to bus (pull D+ high)
        let r = UsbInstance::regs();
        r.devctl().modify(|w| w.set_session(true));
        r.power().modify(|w| {
            w.set_enable_suspend_m(true);
            w.set_hs_enab(false); // Full Speed only — no HS chirp
            w.set_soft_conn(true);
        });

        let power = r.power().read();
        debug!(
            "USB power reg: {} soft_conn={}, suspend_m={}, suspend={}",
            power.0,
            power.soft_conn(),
            power.enable_suspend_m(),
            power.suspend_mode()
        );
        let devctl = r.devctl().read();
        debug!(
            "USB devctl: session={}, vbus={}, host_mode={}",
            devctl.session(),
            devctl.vbus().to_bits(),
            devctl.host_mode()
        );
        debug!("USB device built and attached");
    }

    /// Poll USB hardware — captures interrupts and drives usb-device.
    fn poll(&mut self) {
        if self.device.is_none() {
            return;
        }

        if let (Some(device), Some(class)) = (&mut self.device, &mut self.class) {
            unsafe { musb::on_interrupt::<UsbInstance>() };
            device.poll(&mut [class]);
        }
    }

    fn bus_state(&self) -> BusState {
        match self.device.as_ref().map(|d| d.state()) {
            Some(UsbDeviceState::Default) => BusState::Default,
            Some(UsbDeviceState::Addressed) => BusState::Addressed,
            Some(UsbDeviceState::Configured) => BusState::Configured,
            Some(UsbDeviceState::Suspend) => BusState::Suspended,
            _ => BusState::Detached,
        }
    }

    fn reset(&mut self) {
        // Detach from bus
        UsbInstance::regs()
            .power()
            .modify(|w| w.set_soft_conn(false));

        // Drop usb-device state
        self.device = None;
        self.class = None;
        *USB_ALLOC.get() = None;

        // Power off USB PHY and disable peripheral clock
        sifli_pac::HPSYS_CFG.usbcr().modify(|w| {
            w.set_usb_en(false);
            w.set_dp_en(false);
        });
        sifli_pac::HPSYS_RCC.enr2().modify(|w| w.set_usbc(false));

        *self = Self::new();
    }
}

static USB: SyncCell<UsbGlobal> = SyncCell::new(UsbGlobal::new());

// ---------------------------------------------------------------------------
// UsbBus resource
// ---------------------------------------------------------------------------

struct UsbBusResource;

impl UsbBus for UsbBusResource {
    fn take(_meta: ipc::Meta, identity: DeviceIdentity, endpoints: u8) -> Result<Self, UsbError> {
        let usb = USB.get();
        if usb.bus_taken {
            return Err(UsbError::AlreadyTaken);
        }
        usb.bus_taken = true;
        usb.identity = identity;
        usb.endpoint_count = endpoints;

        // Enable the USB peripheral clock
        let rcc = sifli_pac::HPSYS_RCC;
        rcc.enr2().modify(|w| w.set_usbc(true));

        // Reset the USB controller
        rcc.rstr2().modify(|w| w.set_usbc(true));
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        rcc.rstr2().modify(|w| w.set_usbc(false));

        // USB function clock must be 60MHz: 240MHz / 4 = 60MHz
        rcc.usbcr().modify(|w| w.set_div(4));

        // Power on the USB PHY (LDO + transceiver + D+ pull resistor)
        sifli_pac::HPSYS_CFG.usbcr().modify(|w| {
            w.set_usb_en(true);
            w.set_dp_en(true);
        });
        let phy = sifli_pac::HPSYS_CFG.usbcr().read();
        debug!(
            "USB PHY cr: {} usb_en={}, dp_en={}, ldo_vsel={}, dm_pd={}",
            phy.0,
            phy.usb_en(),
            phy.dp_en(),
            phy.ldo_vsel(),
            phy.dm_pd()
        );

        // Strip all pad config except FSEL on D+/D- — USB PHY is analog,
        // digital pull/input/schmitt/drive settings can interfere
        unsafe {
            let pa35 = 0x5000_30C0 as *mut u32;
            pa35.write_volatile(0x02); // FSEL=2 only
            let pa36 = 0x5000_30C4 as *mut u32;
            pa36.write_volatile(0x02); // FSEL=2 only
        }
        let pa35_pad = unsafe { core::ptr::read_volatile(0x5000_30C0 as *const u32) };
        let pa36_pad = unsafe { core::ptr::read_volatile(0x5000_30C4 as *const u32) };
        debug!("USB pinmux: PA35={}, PA36={}", pa35_pad, pa36_pad);

        // Create the musb bus and usb-device allocator
        *USB_ALLOC.get() = Some(UsbBusAllocator::new(UsbdBus::<UsbInstance>::new()));

        let vid = identity.vendor_id;
        let pid = identity.product_id;
        debug!(
            "USB bus taken: vendor={} product={}, {} endpoints",
            vid, pid, endpoints
        );

        Ok(UsbBusResource)
    }

    fn state(&mut self, _meta: ipc::Meta) -> BusState {
        let usb = USB.get();
        usb.poll();
        usb.bus_state()
    }

    fn take_endpoint_handle(&mut self, _meta: ipc::Meta) -> Option<EndpointHandle> {
        let usb = USB.get();
        let issued = usb.handles_issued;
        if issued >= usb.endpoint_count {
            return None;
        }
        usb.handles_issued = issued + 1;
        Some(EndpointHandle(issued as u32))
    }
}

impl Drop for UsbBusResource {
    fn drop(&mut self) {
        debug!("USB bus released, detaching");
        USB.get().reset();
    }
}

// ---------------------------------------------------------------------------
// UsbEndpoint resource
// ---------------------------------------------------------------------------

struct UsbEndpointResource {
    config: EndpointConfig,
}

impl UsbEndpoint for UsbEndpointResource {
    fn open(
        _meta: ipc::Meta,
        handle: EndpointHandle,
        config: EndpointConfig,
    ) -> Result<Self, UsbError> {
        let usb = USB.get();

        if !usb.bus_taken {
            return Err(UsbError::NotConfigured);
        }

        // Validate and consume handle
        let handle_id = handle.0 as u8;
        if handle_id >= usb.endpoint_count {
            return Err(UsbError::InvalidEndpoint);
        }
        let mask = 1u8 << handle_id;
        if usb.handles_consumed & mask != 0 {
            return Err(UsbError::InvalidEndpoint);
        }
        usb.handles_consumed |= mask;

        // Basic validation
        let ep_num = config.number;
        if ep_num == 0 || ep_num >= 8 {
            return Err(UsbError::InvalidEndpoint);
        }

        // Prevent double-configuring the same hardware endpoint + direction
        let idx = (ep_num - 1) as usize;
        let slot = match config.direction {
            Direction::In => &mut usb.endpoints_in[idx],
            Direction::Out => &mut usb.endpoints_out[idx],
        };
        if slot.is_some() {
            return Err(UsbError::EndpointBusy);
        }

        let ep_dir = config.direction;
        let ep_tt = config.transfer_type;
        let ep_mps = config.max_packet_size;
        debug!(
            "USB EP{} {} {} mps={} (handle {})",
            ep_num, ep_dir, ep_tt, ep_mps, handle.0
        );

        *slot = Some(config);
        usb.endpoints_opened += 1;

        if usb.endpoints_opened == usb.endpoint_count {
            debug!(
                "All {} endpoints configured, building USB device",
                usb.endpoint_count
            );
            usb.activate();
        }

        Ok(UsbEndpointResource { config })
    }

    fn write(
        &mut self,
        _meta: ipc::Meta,
        data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<u16, UsbError> {
        if self.config.direction != Direction::In {
            return Err(UsbError::InvalidEndpoint);
        }

        let usb = USB.get();
        usb.poll();

        if usb.bus_state() != BusState::Configured {
            return Err(UsbError::Disconnected);
        }

        let class = usb.class.as_ref().ok_or(UsbError::NotConfigured)?;
        let idx = (self.config.number - 1) as usize;
        let ep = class.ep_in[idx].as_ref().ok_or(UsbError::InvalidEndpoint)?;

        // Copy lease data into a stack buffer (max 64 bytes per packet)
        let len = data.len().min(self.config.max_packet_size as usize);
        let mut buf = [0u8; 64];
        for (i, byte) in buf.iter_mut().enumerate().take(len) {
            *byte = data.read(i).unwrap_or(0);
        }

        match ep.write(&buf[..len]) {
            Ok(n) => Ok(n as u16),
            Err(usb_device::UsbError::WouldBlock) => Err(UsbError::EndpointBusy),
            Err(_) => Err(UsbError::Disconnected),
        }
    }

    fn read(
        &mut self,
        _meta: ipc::Meta,
        buf_lease: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<u16, UsbError> {
        if self.config.direction != Direction::Out {
            return Err(UsbError::InvalidEndpoint);
        }

        let usb = USB.get();
        usb.poll();

        if usb.bus_state() != BusState::Configured {
            return Err(UsbError::Disconnected);
        }

        let class = usb.class.as_ref().ok_or(UsbError::NotConfigured)?;
        let idx = (self.config.number - 1) as usize;
        let ep = class.ep_out[idx]
            .as_ref()
            .ok_or(UsbError::InvalidEndpoint)?;

        let mut buf = [0u8; 64];
        match ep.read(&mut buf) {
            Ok(n) => {
                let to_copy = n.min(buf_lease.len());
                for (i, &byte) in buf.iter().enumerate().take(to_copy) {
                    let _ = buf_lease.write(i, byte);
                }
                Ok(to_copy as u16)
            }
            Err(usb_device::UsbError::WouldBlock) => Err(UsbError::EndpointBusy),
            Err(_) => Err(UsbError::Disconnected),
        }
    }

    fn set_stall(&mut self, _meta: ipc::Meta, stalled: bool) {
        let ep_num = self.config.number;

        match self.config.direction {
            Direction::In => {
                musb::common_impl::ep_tx_stall::<UsbInstance>(ep_num, stalled);
            }
            Direction::Out => {
                musb::common_impl::ep_rx_stall::<UsbInstance>(ep_num, stalled);
            }
        }
    }
}

impl Drop for UsbEndpointResource {
    fn drop(&mut self) {
        debug!(
            "USB EP{} {} closed",
            self.config.number, self.config.direction
        );
        let usb = USB.get();
        let idx = (self.config.number - 1) as usize;
        match self.config.direction {
            Direction::In => usb.endpoints_in[idx] = None,
            Direction::Out => usb.endpoints_out[idx] = None,
        }
        usb.endpoints_opened -= 1;
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    ipc::server! {
        UsbBus: UsbBusResource,
        UsbEndpoint: UsbEndpointResource,
    }
}
