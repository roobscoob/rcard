#![no_std]
#![no_main]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU8, Ordering};

use generated::slots::SLOTS;
use rcard_log::{error, info, warn};
use sysmodule_bluetooth_api::*;
use trouble_host::prelude::*;

#[allow(dead_code)]
mod gatt;
mod transport;

mod api {
    #[allow(unused_imports)]
    pub use sysmodule_bluetooth_api::*;
}

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);
sysmodule_lcpu_api::bind_lcpu!(Lcpu = SLOTS.sysmodule_lcpu);

pub type BtMutex = ipc::executor::HubrisMutex;

// --- Shared state ---

static CONNECTION_STATE: AtomicU8 = AtomicU8::new(ConnectionState::Disconnected as u8);
static ADV_REQUEST: ipc::executor::Signal<()> = ipc::executor::Signal::new();

struct SyncCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for SyncCell<T> {}
impl<T: Copy> SyncCell<T> {
    const fn new(val: T) -> Self { Self(UnsafeCell::new(val)) }
    fn get(&self) -> T { unsafe { *self.0.get() } }
    fn set(&self, val: T) { unsafe { *self.0.get() = val } }
}

static DEVICE_NAME: SyncCell<[u8; 32]> = SyncCell::new([0u8; 32]);
static DEVICE_NAME_LEN: SyncCell<u8> = SyncCell::new(0);
static LCPU_READY: AtomicU8 = AtomicU8::new(0);

fn get_connection_state() -> ConnectionState {
    match CONNECTION_STATE.load(Ordering::Relaxed) {
        1 => ConnectionState::Advertising,
        2 => ConnectionState::Connected,
        _ => ConnectionState::Disconnected,
    }
}

fn set_connection_state(s: ConnectionState) {
    CONNECTION_STATE.store(s as u8, Ordering::Relaxed);
}

// --- LCPU IPC bridge ---

static LCPU_HANDLE: once_cell::OnceCell<Lcpu> = once_cell::OnceCell::new();

pub(crate) fn lcpu_recv_hci(buf: &mut [u8]) -> usize {
    match LCPU_HANDLE.get() {
        Some(h) => match h.recv_hci(buf) {
            Ok(n) => n as usize,
            Err(_) => 0,
        },
        None => 0,
    }
}

pub(crate) fn lcpu_send_hci(data: &[u8]) -> Result<(), ()> {
    match LCPU_HANDLE.get() {
        Some(h) => match h.send_hci(data) {
            Ok(Ok(())) => Ok(()),
            _ => Err(()),
        },
        None => Err(()),
    }
}

// --- IPC resource ---

struct BluetoothResource;

impl Bluetooth for BluetoothResource {
    fn init(
        _meta: ipc::Meta,
        bd_addr: [u8; 6],
        device_name: [u8; 32],
        name_len: u8,
    ) -> Result<Self, BtError> {
        info!("init starting");

        DEVICE_NAME.set(device_name);
        DEVICE_NAME_LEN.set(name_len);

        let lcpu = match Lcpu::init(bd_addr) {
            Ok(Ok(h)) => h,
            _ => return Err(BtError::LcpuInitFailed),
        };
        let _ = LCPU_HANDLE.set(lcpu);
        LCPU_READY.store(1, Ordering::Release);

        info!("LCPU up, BLE stack ready");
        Ok(BluetoothResource)
    }

    fn start_advertising(&mut self, _meta: ipc::Meta) -> Result<(), BtError> {
        if get_connection_state() == ConnectionState::Advertising {
            return Err(BtError::AlreadyAdvertising);
        }
        ADV_REQUEST.signal(());
        Ok(())
    }

    fn stop_advertising(&mut self, _meta: ipc::Meta) -> Result<(), BtError> {
        Ok(())
    }

    fn write_characteristic(
        &mut self,
        _meta: ipc::Meta,
        _data: [u8; 20],
        _len: u8,
    ) -> Result<(), BtError> {
        Ok(())
    }

    fn read_characteristic(&mut self, _meta: ipc::Meta) -> CharValue {
        CharValue { data: [0u8; 20], len: 0 }
    }

    fn notify(
        &mut self,
        _meta: ipc::Meta,
        _data: [u8; 20],
        _len: u8,
    ) -> Result<(), BtError> {
        if get_connection_state() != ConnectionState::Connected {
            return Err(BtError::NotConnected);
        }
        Ok(())
    }

    fn connection_state(&mut self, _meta: ipc::Meta) -> ConnectionState {
        get_connection_state()
    }
}

impl Drop for BluetoothResource {
    fn drop(&mut self) {
        info!("dropping bluetooth resource");
        set_connection_state(ConnectionState::Disconnected);
    }
}

// --- Reactor notification handler ---
// The LCPU sysmodule posts GROUP_ID_LCPU_DATA when HCI data arrives.
// We wake the transport's read future.

#[ipc::notification_handler(lcpu_data)]
fn handle_lcpu_data(_sender: u16, _code: u32) {
    transport::HCI_RX_SIGNAL.signal(());
}

// --- Entry point ---

#[unsafe(export_name = "main")]
fn main() -> ! {
    info!("starting");
    ipc::async_server! {
        Bluetooth: BluetoothResource,
        @notifications(Reactor) => handle_lcpu_data,
        @spawn => [
            async {
                info!("async: waiting for LCPU init");
                loop {
                    if LCPU_READY.load(Ordering::Acquire) != 0 {
                        break;
                    }
                    embassy_time::Timer::after_millis(100).await;
                }
                info!("async: building trouble host");

                let controller = ExternalController::<_, 4>::new(transport::HciTransport);

                static RESOURCES: static_cell::StaticCell<HostResources<DefaultPacketPool, 1, 1>> =
                    static_cell::StaticCell::new();
                let resources = RESOURCES.init(HostResources::new());

                let stack = trouble_host::new(controller, resources);
                let Host {
                    mut peripheral,
                    mut runner,
                    ..
                } = stack.build();

                embassy_futures::join::join(
                    async {
                        info!("runner starting");
                        let _ = runner.run().await;
                        warn!("runner exited");
                    },
                    async {
                        loop {
                            ADV_REQUEST.wait().await;
                            info!("starting advertising");
                            set_connection_state(ConnectionState::Advertising);

                            let name = DEVICE_NAME.get();
                            let name_len = DEVICE_NAME_LEN.get() as usize;

                            let params = AdvertisementParameters::default();

                            // Build raw advertisement data bytes
                            let mut adv_buf = [0u8; 31];
                            let mut pos = 0;
                            // Flags: length=2, type=0x01, value=0x06
                            adv_buf[pos] = 2;
                            adv_buf[pos + 1] = 0x01;
                            adv_buf[pos + 2] = 0x06;
                            pos += 3;
                            // Complete Local Name: length=1+name_len, type=0x09
                            adv_buf[pos] = (name_len + 1) as u8;
                            adv_buf[pos + 1] = 0x09;
                            adv_buf[pos + 2..pos + 2 + name_len].copy_from_slice(&name[..name_len]);
                            pos += 2 + name_len;

                            let adv = Advertisement::ConnectableScannableUndirected {
                                adv_data: &adv_buf[..pos],
                                scan_data: &[],
                            };

                            let advertiser = match peripheral.advertise(&params, adv).await {
                                Ok(adv) => adv,
                                Err(_) => {
                                    error!("advertise failed");
                                    set_connection_state(ConnectionState::Disconnected);
                                    continue;
                                }
                            };

                            info!("waiting for connection");
                            match advertiser.accept().await {
                                Ok(conn) => {
                                    info!("connected");
                                    set_connection_state(ConnectionState::Connected);
                                    let _ = conn;
                                }
                                Err(_) => {
                                    error!("accept failed");
                                }
                            }

                            set_connection_state(ConnectionState::Disconnected);
                            info!("disconnected");
                        }
                    },
                ).await;
            }
        ],
    }
}
