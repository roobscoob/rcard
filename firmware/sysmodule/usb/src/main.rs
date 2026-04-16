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
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

use generated::notifications;
use sysmodule_reactor_api::OverflowStrategy;

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
    /// Interface numbers keyed by `interface_group`. Endpoints with the
    /// same group share a USB interface even when they use different
    /// hardware endpoint numbers (required on shared-FIFO parts like
    /// sf32lb52x where IN and OUT must live on separate endpoints).
    ifaces: [Option<(u8, usb_device::bus::InterfaceNumber)>; 7],
    ep_in: [Option<EndpointIn<'a, B>>; 7],
    ep_out: [Option<EndpointOut<'a, B>>; 7],
    /// Which interface_group each endpoint index belongs to (for descriptor
    /// generation). Indexed by ep_num-1, mirrors endpoints_in/out.
    ep_group_in: [Option<u8>; 7],
    ep_group_out: [Option<u8>; 7],
    /// Identity blob reborrowed from `UsbGlobal` with `'static` lifetime.
    /// Walked on each BOS / vendor-request enumeration.
    identity: DeviceBlob<'static>,
}

impl<'a> VendorClass<'a, UsbdBus<UsbInstance>> {
    fn new(
        alloc: &'a UsbBusAllocator<UsbdBus<UsbInstance>>,
        endpoints_in: &[Option<EndpointConfig>; 7],
        endpoints_out: &[Option<EndpointConfig>; 7],
        identity: DeviceBlob<'static>,
    ) -> Self {
        let mut class = VendorClass {
            ifaces: [const { None }; 7],
            ep_in: [const { None }; 7],
            ep_out: [const { None }; 7],
            ep_group_in: [None; 7],
            ep_group_out: [None; 7],
            identity,
        };

        // Helper: find or allocate an InterfaceNumber for a given group.
        fn get_or_alloc_iface(
            ifaces: &mut [Option<(u8, usb_device::bus::InterfaceNumber)>; 7],
            alloc: &UsbBusAllocator<UsbdBus<UsbInstance>>,
            group: u8,
        ) -> usb_device::bus::InterfaceNumber {
            for entry in ifaces.iter() {
                if let Some((g, iface)) = entry {
                    if *g == group {
                        return *iface;
                    }
                }
            }
            let iface = alloc.interface();
            for entry in ifaces.iter_mut() {
                if entry.is_none() {
                    *entry = Some((group, iface));
                    return iface;
                }
            }
            panic!("too many interface groups");
        }

        for idx in 0..7 {
            let has_in = endpoints_in[idx].is_some();
            let has_out = endpoints_out[idx].is_some();

            if !has_in && !has_out {
                continue;
            }

            let ep_num = (idx + 1) as u8;

            if let Some(config) = &endpoints_in[idx] {
                get_or_alloc_iface(&mut class.ifaces, alloc, config.interface_group);
                class.ep_group_in[idx] = Some(config.interface_group);
                let ep_type = transfer_type(config.transfer_type);
                let addr =
                    EndpointAddress::from_parts(ep_num as usize, usb_device::UsbDirection::In);
                class.ep_in[idx] = Some(
                    alloc
                        .alloc(Some(addr), ep_type, config.max_packet_size, config.interval)
                        .map_err(UsbdError::from)
                        .log_expect("EP IN alloc failed"),
                );
            }

            if let Some(config) = &endpoints_out[idx] {
                get_or_alloc_iface(&mut class.ifaces, alloc, config.interface_group);
                class.ep_group_out[idx] = Some(config.interface_group);
                let ep_type = transfer_type(config.transfer_type);
                let addr =
                    EndpointAddress::from_parts(ep_num as usize, usb_device::UsbDirection::Out);
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
        // Emit one interface descriptor per interface_group, then all
        // endpoints belonging to that group (potentially from different
        // hardware endpoint numbers on shared-FIFO parts).
        for &entry in self.ifaces.iter() {
            let Some((group, iface)) = entry else { continue };
            writer.interface(iface, 0xFF, 0x00, 0x00)?;
            for idx in 0..7 {
                if self.ep_group_out[idx] == Some(group) {
                    if let Some(ep) = &self.ep_out[idx] {
                        writer.endpoint(ep)?;
                    }
                }
                if self.ep_group_in[idx] == Some(group) {
                    if let Some(ep) = &self.ep_in[idx] {
                        writer.endpoint(ep)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn get_bos_descriptors(
        &self,
        writer: &mut usb_device::descriptor::BosWriter,
    ) -> usb_device::Result<()> {
        for (cap_type, payload) in self.identity.bos_capabilities() {
            writer.capability(cap_type, payload)?;
        }
        Ok(())
    }

    fn control_in(&mut self, xfer: usb_device::class::ControlIn<B>) {
        let req = xfer.request();
        if req.request_type == usb_device::control::RequestType::Vendor {
            if let Some(response) = self.identity.vendor_request(req.request, req.index) {
                let _ = xfer.accept_with(response);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Global sysmodule state
// ---------------------------------------------------------------------------

/// Maximum size of the identity blob copied out of the `UsbBus::take` lease.
/// Sized to fit strings + a couple of BOS capabilities + vendor request
/// responses without being wasteful. A basic MSOS-enabled device uses ~130
/// bytes; 2 KiB leaves headroom for WebUSB and future additions.
const IDENTITY_BLOB_SIZE: usize = 2048;

struct UsbGlobal {
    config: FixedDeviceConfig,
    identity_blob: [u8; IDENTITY_BLOB_SIZE],
    identity_len: usize,
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
            config: FixedDeviceConfig {
                vendor_id: 0,
                product_id: 0,
                device_class: 0,
                device_subclass: 0,
                device_protocol: 0,
                bcd_device: 0,
            },
            identity_blob: [0; IDENTITY_BLOB_SIZE],
            identity_len: 0,
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

    /// Reborrow the identity blob with `'static` lifetime. Sound because
    /// `UsbGlobal` lives in `static USB` and is never moved; the blob is
    /// treated as immutable for the duration of the bus session, and
    /// `reset()` only runs after `self.class`/`self.device` (the holders)
    /// have been dropped.
    fn identity_static(&self) -> DeviceBlob<'static> {
        let slice =
            unsafe { core::slice::from_raw_parts(self.identity_blob.as_ptr(), self.identity_len) };
        DeviceBlob(slice)
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

        // Reborrow the identity blob as `'static` — sound because UsbGlobal
        // lives in a `static`. This replaces the earlier `addr_of!(self.identity)`
        // hack and sidesteps borrow-checker conflicts with `self.poll()` below.
        let identity = self.identity_static();

        // Create our class (allocates endpoints from usb-device)
        let class = VendorClass::new(alloc, &self.endpoints_in, &self.endpoints_out, identity);

        let mut strings = usb_device::device::StringDescriptors::default();
        if let Some(m) = identity.manufacturer() {
            strings = strings.manufacturer(m);
        }
        if let Some(p) = identity.product() {
            strings = strings.product(p);
        }
        if let Some(s) = identity.serial() {
            strings = strings.serial_number(s);
        }

        // Packed-struct fields require copies before use.
        let vid = self.config.vendor_id;
        let pid = self.config.product_id;
        let dev_class = self.config.device_class;
        let dev_subclass = self.config.device_subclass;
        let dev_protocol = self.config.device_protocol;
        let bcd = self.config.bcd_device;

        let device = UsbDeviceBuilder::new(alloc, UsbVidPid(vid, pid))
            .usb_rev(if identity.has_bos() {
                UsbRev::Usb210
            } else {
                UsbRev::Usb200
            })
            .strings(&[strings])
            .map_err(BuilderErr::from)
            .log_expect("string descriptors failed")
            .device_class(dev_class)
            .device_sub_class(dev_subclass)
            .device_protocol(dev_protocol)
            .device_release(bcd)
            .self_powered(true)
            .max_power(500)
            .map_err(BuilderErr::from)
            .log_expect("EP0 alloc failed")
            .max_packet_size_0(64)
            .map_err(BuilderErr::from)
            .log_expect("EP0 alloc failed")
            .build();

        self.class = Some(class);
        self.device = Some(device);

        // Enable MUSB interrupt status registers (required for on_interrupt
        // to see bus events — intrusb/intrtx/intrrx are gated by these)
        musb::common_impl::bus_init::<UsbInstance>();

        // Disable Double Packet Buffering on every non-EP0 endpoint, mirroring
        // SiFli's CherryUSB glue (port/musb/usb_glue_sifli.c, gated `#ifndef
        // SOC_SF32LB55X`). The 52X/56X/58X family ships with broken DPB
        // hardware and the vendor explicitly turns it off at init. The musb
        // crate has these helpers but never calls them. Passing `0` means
        // "no endpoints have DPB enabled" — the helper inverts and masks to
        // write 0xFFE to dpktbufdis (bits 1..11), preserving EP0.
        musb::common_impl::endpoints_set_tx_dualpacket_enabled::<UsbInstance>(0);
        musb::common_impl::endpoints_set_rx_dualpacket_enabled::<UsbInstance>(0);

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
        info!("{} {}", "usb: activate state", self.bus_state());
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
    fn take(
        _meta: ipc::Meta,
        config: FixedDeviceConfig,
        blob: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
        endpoints: u8,
    ) -> Result<Self, UsbError> {
        let usb = USB.get();
        if usb.bus_taken {
            return Err(UsbError::AlreadyTaken);
        }

        // Copy the identity blob into our arena in small chunks.
        let blob_len = blob.len();
        if blob_len > usb.identity_blob.len() {
            return Err(UsbError::BufferOverflow);
        }
        {
            let mut tmp = [0u8; 64];
            let mut off = 0;
            while off < blob_len {
                let n = (blob_len - off).min(tmp.len());
                let _ = blob.read_range(off, &mut tmp[..n]);
                usb.identity_blob[off..off + n].copy_from_slice(&tmp[..n]);
                off += n;
            }
        }
        usb.identity_len = blob_len;

        // Reject malformed blobs up-front so the server never serves a
        // truncated TLV entry during enumeration.
        if !DeviceBlob(&usb.identity_blob[..usb.identity_len]).validate() {
            usb.identity_len = 0;
            return Err(UsbError::MalformedIdentity);
        }

        usb.bus_taken = true;
        usb.config = config;
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

        let vid = config.vendor_id;
        let pid = config.product_id;
        debug!(
            "USB bus taken: vendor={} product={}, {} endpoints",
            vid, pid, endpoints
        );

        Ok(UsbBusResource)
    }

    fn state(&mut self, _meta: ipc::Meta) -> BusState {
        // No need to poll here — the @irq handler drives the stack.
        USB.get().bus_state()
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

        if usb.bus_state() != BusState::Configured {
            return Err(UsbError::Disconnected);
        }

        let class = usb.class.as_ref().ok_or(UsbError::NotConfigured)?;
        let idx = (self.config.number - 1) as usize;
        let ep = class.ep_in[idx].as_ref().ok_or(UsbError::InvalidEndpoint)?;

        // Copy lease data into a stack buffer (max 64 bytes per packet).
        // Use a single bulk borrow_read syscall instead of `len` per-byte
        // syscalls — both faster and avoids the corruption-prone byte loop.
        let len = data.len().min(self.config.max_packet_size as usize);
        let mut buf = [0u8; 64];
        let _ = data.read_range(0, &mut buf[..len]);

        let result = ep.write(&buf[..len]);

        match result {
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
                // Single bulk borrow_write syscall instead of `n` per-byte
                // syscalls — same rationale as the bulk read in `write()`.
                let to_copy = n.min(buf_lease.len());
                let _ = buf_lease.write_range(0, &buf[..to_copy]);
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
        usb.endpoints_opened = usb.endpoints_opened.saturating_sub(1);
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
        @irq(usbc_irq) => || {
            USB.get().poll();
            // Wake any task subscribed to usb_event so it can drain
            // endpoints or re-query bus state. `refresh` coalesces
            // repeated IRQs into a single pending notification.
            let _ = Reactor::refresh(
                notifications::GROUP_ID_USB_EVENT,
                0,
                15,
                OverflowStrategy::DropOldest,
            );
        },
    }
}
