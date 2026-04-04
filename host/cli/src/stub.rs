/// The stub firmware archive, built at compile time from `firmware/build.nu --features stub`.
///
/// This is loaded into device RAM via the debug interface before flashing
/// the real firmware.
pub const TFW: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stub.tfw"));
