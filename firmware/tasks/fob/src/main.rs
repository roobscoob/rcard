#![no_std]
#![no_main]

use generated::slots::SLOTS;
use rcard_log::{ResultExt, info};
use sysmodule_lcpu_api::*;
use sysmodule_reactor_api::NOTIFICATION_BIT;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(to Log);

sysmodule_lcpu_api::bind_lcpu!(Lcpu = SLOTS.sysmodule_lcpu);
sysmodule_reactor_api::bind_reactor!(Reactor = SLOTS.sysmodule_reactor);

// ── HCI command frames (H4 type byte + opcode + param_len + params) ──

/// HCI_Reset (OGF=0x03, OCF=0x0003 → opcode 0x0C03), no params.
const HCI_RESET: &[u8] = &[0x01, 0x03, 0x0C, 0x00];

/// HCI_LE_Set_Advertising_Parameters (opcode 0x2006).
/// min/max interval = 0x00A0 (= 100 ms in 0.625 ms units), ADV_IND,
/// public address, channels 37/38/39, allow any.
const HCI_LE_SET_ADV_PARAMS: &[u8] = &[
    0x01, 0x06, 0x20, 0x0F, // H4 cmd, opcode lo/hi, param_len = 15
    0xA0, 0x00, // adv_interval_min
    0xA0, 0x00, // adv_interval_max
    0x00, // adv_type = ADV_IND (connectable undirected)
    0x00, // own_address_type = public
    0x00, // peer_address_type = public
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // peer_address (unused with filter=0)
    0x07, // channel_map = ch 37/38/39
    0x00, // filter_policy = allow any
];

/// HCI_LE_Set_Advertising_Data (opcode 0x2008).
/// 10 bytes used: Flags AD + Complete Local Name "Charm". Remainder zero.
const HCI_LE_SET_ADV_DATA: &[u8] = &[
    0x01, 0x08, 0x20, 0x20, // H4 cmd, opcode lo/hi, param_len = 32
    0x0A, // adv_data_length = 10
    // AD #1: Flags (LE General Discoverable + BR/EDR not supported)
    0x02, 0x01, 0x06,
    // AD #2: Complete Local Name "Charm"
    0x06, 0x09, b'C', b'h', b'a', b'r', b'm',
    // 21 zero bytes of padding to reach the fixed 31-byte adv_data field
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// HCI_LE_Set_Advertising_Enable (opcode 0x200A), enable = 0x01.
const HCI_LE_SET_ADV_ENABLE: &[u8] = &[0x01, 0x0A, 0x20, 0x01, 0x01];

const OP_RESET: u16 = 0x0C03;
const OP_LE_SET_ADV_PARAMS: u16 = 0x2006;
const OP_LE_SET_ADV_DATA: u16 = 0x2008;
const OP_LE_SET_ADV_ENABLE: u16 = 0x200A;

/// Scan a recv'd HCI byte stream for a Command Complete event matching
/// `expected_opcode`. Returns the status byte if found. Handles multiple
/// concatenated events (we've seen LCPU coalesce two CCs in a single
/// recv when responses pile up).
fn find_cc(buf: &[u8], expected_opcode: u16) -> Option<u8> {
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] != 0x04 {
            return None; // not an HCI Event packet; bail
        }
        let evt_code = buf[i + 1];
        let param_len = buf[i + 2] as usize;
        let next = i + 3 + param_len;
        if next > buf.len() {
            return None;
        }
        if evt_code == 0x0E && param_len >= 4 {
            // Command Complete: num_hci_command_packets, opcode_lo, opcode_hi, status
            let cc_opcode = u16::from_le_bytes([buf[i + 4], buf[i + 5]]);
            if cc_opcode == expected_opcode {
                return Some(buf[i + 6]);
            }
        }
        i = next;
    }
    None
}

/// Drain whatever the reactor has queued so the queue doesn't fill up.
/// We don't care about the notification details — the bare bit waking
/// us is enough.
fn drain_reactor() {
    loop {
        match Reactor::pull() {
            Ok(Some(_)) => {}
            _ => break,
        }
    }
}

/// Send one HCI command and block until we see the matching Command
/// Complete. Logs along the way so we can see exactly where things
/// stall. Gives up silently on IPC errors — this is a debug hack.
fn send_and_await(lcpu: &mut Lcpu, cmd: &[u8], expected_opcode: u16) {
    info!("fob: sending opcode {}", expected_opcode);
    match lcpu.send_hci(cmd) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            info!("fob: send err: {}", e);
            return;
        }
        Err(e) => {
            info!("fob: send ipc err: {}", e);
            return;
        }
    }

    let mut buf = [0u8; 256];
    loop {
        let _ = userlib::sys_recv_notification(NOTIFICATION_BIT);
        drain_reactor();

        // Drain HCI bytes; LCPU may have written multiple events.
        loop {
            let n = match lcpu.recv_hci(&mut buf) {
                Ok(n) => n as usize,
                Err(_) => 0,
            };
            if n == 0 {
                break;
            }
            info!("fob: recv {} bytes", n);
            info!("fob: bytes: {}", buf[..n]);
            if let Some(status) = find_cc(&buf[..n], expected_opcode) {
                info!("fob: CC op={} status={}", expected_opcode, status);
                return;
            }
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    info!("fob: awake");

    let bd_addr = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc];
    let mut lcpu = Lcpu::init(bd_addr)
        .log_expect("lcpu init ipc")
        .log_expect("lcpu init");
    info!("fob: lcpu ready");

    send_and_await(&mut lcpu, HCI_RESET, OP_RESET);
    send_and_await(&mut lcpu, HCI_LE_SET_ADV_PARAMS, OP_LE_SET_ADV_PARAMS);
    send_and_await(&mut lcpu, HCI_LE_SET_ADV_DATA, OP_LE_SET_ADV_DATA);
    send_and_await(&mut lcpu, HCI_LE_SET_ADV_ENABLE, OP_LE_SET_ADV_ENABLE);

    info!("fob: advertising!");

    // Idle loop: drain whatever LCPU posts so the reactor doesn't fill,
    // and log incoming events for visibility (connect/disconnect, etc).
    let mut buf = [0u8; 256];
    loop {
        let _ = userlib::sys_recv_notification(NOTIFICATION_BIT);
        drain_reactor();
        loop {
            let n = match lcpu.recv_hci(&mut buf) {
                Ok(n) => n as usize,
                Err(_) => 0,
            };
            if n == 0 {
                break;
            }
            info!("fob: post-adv recv {} bytes: {}", n, buf[..n]);
        }
    }
}
