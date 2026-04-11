#![no_std]

/// Length (in bytes) of the ASCII hex rendering of a 16-byte chip UID.
pub const UID_HEX_LEN: usize = 32;

/// Render a 16-byte chip UID as a 32-character uppercase hex string.
///
/// Writes into `buf` and returns a `&str` borrowed from it — the caller
/// owns the buffer, so the resulting `&str` is valid for as long as
/// `buf` is. Intended for wiring the UID into places that want a
/// `&str`, like the USB `DeviceIdentity::serial` descriptor.
pub fn uid_to_hex<'a>(uid: &[u8; 16], buf: &'a mut [u8; UID_HEX_LEN]) -> &'a str {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for (i, byte) in uid.iter().enumerate() {
        buf[i * 2] = HEX[(byte >> 4) as usize];
        buf[i * 2 + 1] = HEX[(byte & 0x0F) as usize];
    }
    // SAFETY: every byte we just wrote is in the ASCII hex alphabet.
    unsafe { core::str::from_utf8_unchecked(buf) }
}

#[ipc::resource(arena_size = 0, kind = 0x07)]
pub trait DeviceInfo {
    /// Return the device's 16-byte chip UID, read once at boot from
    /// eFuse bank 0 via `sysmodule_efuse`.
    #[message]
    fn get_uid() -> [u8; 16];
}