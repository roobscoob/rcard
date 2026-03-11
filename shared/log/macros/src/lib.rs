mod attrs;
mod derive_format;
mod species;

use proc_macro::TokenStream;

/// Derive the `Format` trait for structs and enums.
///
/// Generates a `Format` impl and emits metadata statics into the `.rcard_log`
/// linker section. The addresses of those statics become type_ids and field_ids
/// on the wire.
///
/// Supports `#[format(key = "value")]` hint attributes on both the type and
/// individual fields. Hints are opaque key-value pairs forwarded to the host
/// renderer via the linker section metadata.
#[proc_macro_derive(Format, attributes(format))]
pub fn derive_format(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    derive_format::derive(input).into()
}

/// Internal macro used by the `info!`, `warn!`, etc. log macros.
///
/// Generates a `.rcard_log` species static and serializes arguments.
/// Not intended for direct use — use the level macros instead.
#[proc_macro]
pub fn __species(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as species::SpeciesInput);
    species::expand_species(input).into()
}
