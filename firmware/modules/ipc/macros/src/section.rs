//! Emit IPC interface metadata into an ELF `.ipc_meta` INFO section.
//!
//! Mirrors the pattern used by `rcard_log_macros::section` for log
//! metadata: the proc macro serializes a JSON value to bytes,
//! null-terminates it, and emits a `#[link_section = ".ipc_meta"]`
//! `#[used]` byte array static. The build pipeline reads these sections
//! from the final task ELFs and collates them into `ipc-metadata.json`.
//!
//! Each `#[ipc::resource]`, `#[ipc::interface]`, and `ipc::server!`
//! expansion emits one record. The scraper splits on null bytes and
//! parses each chunk as one JSON value.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

static MEMBER_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Emit a JSON metadata entry into `.ipc_meta`.
///
/// `tag` is a short identifier used only to construct a unique static
/// name so multiple emissions in the same module don't collide.
pub fn emit(tag: &str, value: &serde_json::Value) -> TokenStream {
    let mut bytes: Vec<u8> = serde_json::to_vec(value)
        .unwrap_or_else(|e| panic!("failed to serialize ipc metadata: {e}"));
    bytes.push(0); // null terminator — the scraper splits on these
    let len = bytes.len();

    let counter = MEMBER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let static_name = format_ident!("__ipc_meta_{}_{}", tag, counter);

    quote! {
        const _: () = {
            #[unsafe(link_section = ".ipc_meta")]
            #[used]
            #[allow(non_upper_case_globals)]
            static #static_name: [u8; #len] = [#(#bytes),*];
        };
    }
}
