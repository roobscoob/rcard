//! Hardcoded GATT attribute table for first-cut BLE peripheral.

use trouble_host::prelude::*;
use crate::BtMutex;

const CUSTOM_SERVICE_UUID: Uuid =
    Uuid::new_long([0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0,
                     0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0]);

const CUSTOM_CHAR_UUID: Uuid =
    Uuid::new_long([0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0,
                     0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0x01]);

pub struct GattTable {
    pub table: AttributeTable<'static, BtMutex, 16>,
    pub custom_char: Characteristic<u8>,
}

pub fn build(storage: &'static mut [u8]) -> GattTable {
    let mut table: AttributeTable<'_, BtMutex, 16> = AttributeTable::new();

    // Scope the mutable borrows so they end before we move table.
    let custom_char;
    {
        let mut dis = table.add_service(Service::new(0x180au16));
        let _ = dis.add_characteristic_ro(0x2a29u16, b"rcard");
        let _ = dis.add_characteristic_ro(0x2a24u16, b"Charm");
        let _ = dis.add_characteristic_ro(0x2a26u16, b"0.1.0");
    }
    {
        let mut custom = table.add_service(Service::new(CUSTOM_SERVICE_UUID));
        custom_char = custom.add_characteristic(
            CUSTOM_CHAR_UUID,
            &[CharacteristicProp::Read, CharacteristicProp::Write, CharacteristicProp::Notify],
            0u8,
            storage,
        ).build();
    }

    GattTable {
        table,
        custom_char,
    }
}
