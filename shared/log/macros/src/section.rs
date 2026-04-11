//! Emit log metadata into an ELF `.log_strings` INFO section.
//!
//! Replaces the old sidecar JSON file approach. The proc macro serializes
//! metadata as JSON bytes and emits a `#[link_section = ".log_strings"]`
//! static. The build pipeline reads these sections from the final task
//! ELFs instead of globbing sidecar files.

use proc_macro2::TokenStream;
use quote::quote;

/// Emit a JSON metadata entry as a `#[link_section = ".log_strings"]` static.
///
/// The JSON value is serialized to bytes, null-terminated, and emitted as a
/// byte array literal. Wrapped in `const _: () = { ... }` to avoid name
/// collisions across multiple call sites.
pub fn emit(value: &serde_json::Value) -> TokenStream {
    let mut bytes: Vec<u8> = serde_json::to_vec(value)
        .unwrap_or_else(|e| panic!("failed to serialize log metadata: {e}"));
    bytes.push(0); // null terminator — the scraper splits on these
    let len = bytes.len();

    quote! {
        const _: () = {
            #[link_section = ".log_strings"]
            #[used]
            #[allow(non_upper_case_globals)]
            static __log_meta: [u8; #len] = [#(#bytes),*];
        };
    }
}
