#![no_std]

// use postcard_schema::Schema;
// use rcard_log::Format;

#[ipc::resource(arena_size = 8, kind = 0x20)]
pub trait Demo {
    #[message]
    fn hello() -> ();
}

// #[derive(
//     Copy,
//     Clone,
//     Debug,
//     zerocopy::TryFromBytes,
//     zerocopy::IntoBytes,
//     zerocopy::KnownLayout,
//     zerocopy::Immutable,
//     Format,
//     serde::Serialize,
//     serde::Deserialize,
//     Schema,
// )]
// #[repr(u8)]
// pub enum DemoError {
//     Failed,
// }
