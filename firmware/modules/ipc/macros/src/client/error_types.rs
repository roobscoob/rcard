use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::parse::{HandleMode, MethodKind, ParsedParam};

/// Determine the precise client-side error type for a method based on its
/// kind and whether it has move/clone handle params.
///
/// The matrix is:
///   method kind × (has_move, has_clone) → error type
///
/// This gives callers maximally informative errors without requiring them to
/// handle impossible variants.
pub fn error_type_for(method_kind: MethodKind, params: &[ParsedParam]) -> TokenStream2 {
    let has_move = params
        .iter()
        .any(|p| p.handle_mode == Some(HandleMode::Move));
    let has_clone = params
        .iter()
        .any(|p| p.handle_mode == Some(HandleMode::Clone));

    match method_kind {
        MethodKind::Constructor => match (has_move, has_clone) {
            (false, false) => quote! { ipc::errors::ConstructorError },
            (true, false) => quote! { ipc::errors::ConstructorTransferError },
            (false, true) => quote! { ipc::errors::ConstructorCloneError },
            (true, true) => quote! { ipc::errors::ConstructorTransferCloneError },
        },
        MethodKind::Message | MethodKind::Destructor => match (has_move, has_clone) {
            (false, false) => quote! { ipc::errors::HandleLostError },
            (true, false) => quote! { ipc::errors::MessageTransferError },
            (false, true) => quote! { ipc::errors::MessageCloneError },
            (true, true) => quote! { ipc::errors::MessageTransferCloneError },
        },
        MethodKind::StaticMessage => match (has_move, has_clone) {
            (false, false) => quote! { ipc::errors::StaticMessageError },
            (true, false) => quote! { ipc::errors::StaticMessageTransferError },
            (false, true) => quote! { ipc::errors::StaticMessageCloneError },
            (true, true) => quote! { ipc::errors::StaticMessageTransferCloneError },
        },
    }
}
