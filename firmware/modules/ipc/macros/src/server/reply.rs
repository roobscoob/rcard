use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::Ident;

use crate::parse::ParsedMethod;
use crate::util::replace_ident_in_type;
use crate::wire_format;

/// Compute the effective return type for a method's server-side serialization.
///
/// If the method constructs another resource, the generic ident is replaced
/// with `ipc::RawHandle` in the return type.
pub fn server_return_type(m: &ParsedMethod) -> Option<syn::Type> {
    let rt = m.return_type.as_ref()?;
    if let Some((_trait_name, generic_ident)) = &m.constructs {
        let replaced = replace_ident_in_type(rt, generic_ident, &quote! { ipc::RawHandle });
        Some(syn::parse2(replaced).expect("ipc: failed to parse replaced return type"))
    } else {
        Some(rt.clone())
    }
}

/// Reply with `Err(ipc::Error::HandleLost)`.
pub fn gen_reply_handle_lost() -> TokenStream2 {
    quote! {
        reply.reply_ok(&[1u8, ipc::Error::HandleLost as u8]);
        return;
    }
}

/// Generate the reply logic for an instance method (message or destructor).
///
/// Looks up the resource in the arena, calls the method, encodes the return
/// value, and replies.
pub fn gen_reply(
    method_name: &Ident,
    call_args: &[TokenStream2],
    return_type: Option<&syn::Type>,
    is_destructor: bool,
) -> TokenStream2 {
    let arena_op = if is_destructor {
        quote! { self.arena.remove_owned(handle, sender_index) }
    } else {
        quote! { self.arena.get_mut_owned(handle, sender_index) }
    };

    let handle_lost = gen_reply_handle_lost();

    if let Some(rt) = return_type {
        let encode = wire_format::gen_encode_return_value(rt, quote! { result_value });
        quote! {
            let Some(resource) = #arena_op else {
                #handle_lost
            };
            let result_value = resource.#method_name(meta, #(#call_args),*);
            let mut __reply_buf: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
                unsafe { core::mem::MaybeUninit::uninit().assume_init() };
            let mut __off = 0usize;
            ipc::wire::set_uninit(&mut __reply_buf, __off, 0); // Result::Ok tag
            __off += 1;
            #encode
            reply.reply_ok(unsafe { ipc::wire::assume_init_slice(&__reply_buf, __off) });
        }
    } else {
        quote! {
            let Some(resource) = #arena_op else {
                #handle_lost
            };
            resource.#method_name(meta, #(#call_args),*);
            reply.reply_ok(&[0u8]); // Result::Ok(())
        }
    }
}

/// Generate the reply logic for a static method (no arena lookup).
pub fn gen_static_reply(
    method_name: &Ident,
    call_args: &[TokenStream2],
    return_type: Option<&syn::Type>,
) -> TokenStream2 {
    if let Some(rt) = return_type {
        let encode = wire_format::gen_encode_return_value(rt, quote! { result_value });
        quote! {
            let result_value = T::#method_name(meta, #(#call_args),*);
            let mut __reply_buf: [core::mem::MaybeUninit<u8>; ipc::HUBRIS_MESSAGE_SIZE_LIMIT] =
                unsafe { core::mem::MaybeUninit::uninit().assume_init() };
            let mut __off = 0usize;
            ipc::wire::set_uninit(&mut __reply_buf, __off, 0); // Result::Ok tag
            __off += 1;
            #encode
            reply.reply_ok(unsafe { ipc::wire::assume_init_slice(&__reply_buf, __off) });
        }
    } else {
        quote! {
            T::#method_name(meta, #(#call_args),*);
            reply.reply_ok(&[0u8]); // Result::Ok(())
        }
    }
}
