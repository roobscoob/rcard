/// The stub firmware archive, built at compile time.
///
/// This is loaded into device RAM via SifliDebug during the flash process.
pub const TFW: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/stub.tfw"));
