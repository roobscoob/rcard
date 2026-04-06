use quote::format_ident;
use syn::Ident;

use crate::parse::{HandleMode, ParsedParam};
use crate::util::{to_screaming_snake_case, to_snake_case};

/// Check if a parameter is a resolvable peer (clone + impl Trait).
pub fn is_resolvable_peer(p: &ParsedParam) -> bool {
    p.handle_mode == Some(HandleMode::Clone) && p.impl_trait_name.is_some()
}

pub fn peer_generic_name(trait_name: &Ident) -> Ident {
    format_ident!("Peer{}", trait_name)
}

pub fn peer_field_name(trait_name: &Ident) -> Ident {
    format_ident!("peer_{}", to_snake_case(&trait_name.to_string()))
}

pub fn peer_const_name(trait_name: &Ident) -> Ident {
    format_ident!(
        "__PEER_{}_N",
        to_screaming_snake_case(&trait_name.to_string())
    )
}
