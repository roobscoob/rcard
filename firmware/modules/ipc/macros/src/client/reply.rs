use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::parse::ConstructorReturn;
use crate::util::panic_path;
use crate::wire_format;

/// Parse a reply from the server on the client side.
///
/// Wire format: tag byte (0=Ok, 1=Err) + payload.
/// The Ok payload may itself be `Option<T>` or `Result<T, E>`, handled by
/// `wire_format::gen_decode_return_value`.
pub fn gen_parse_reply(return_type: Option<&syn::Type>, server_expr: TokenStream2) -> TokenStream2 {
    if let Some(rt) = return_type {
        let decode = wire_format::gen_decode_return_value(rt, &server_expr);
        quote! {
            match ipc::parse_reply_envelope(retbuffer, len, #server_expr) {
                Ok(mut __off) => {
                    let __decoded = #decode;
                    Ok(__decoded)
                }
                Err(e) => Err(e),
            }
        }
    } else {
        quote! {
            match ipc::parse_reply_envelope(retbuffer, len, #server_expr) {
                Ok(_) => Ok(()),
                Err(e) => Err(e),
            }
        }
    }
}

/// Generate inline tag+payload decoding for constructor replies.
///
/// Wire format from server:
///   - Bare:       `tag(0=Ok,1=Err) + RawHandle | Error`
///   - Result(E):  `tag(0=Ok,1=Err) + (tag(0=Ok,1=Err) + RawHandle | E) | Error`
///   - OptionSelf: `tag(0=Ok,1=Err) + (tag(0=Some,1=None) + RawHandle | ()) | Error`
pub fn gen_ctor_reply(
    ctor_return: &ConstructorReturn,
    err_type: &TokenStream2,
    make_self: &TokenStream2,
) -> (TokenStream2, TokenStream2) {
    let _p = panic_path();

    // All constructor variants share the same outer envelope via
    // parse_reply_envelope. Only the inner payload parsing differs.
    let parse_handle = quote! {
        let Some(__payload) = retbuffer.get(__off..len) else {
            #_p!("ipc: ctor reply truncated");
        };
        let Ok((handle, _)) =
            ipc::__postcard::take_from_bytes::<ipc::RawHandle>(__payload)
        else {
            #_p!("ipc: ctor reply decode failed");
        };
    };

    match ctor_return {
        ConstructorReturn::Bare => (
            quote! { core::result::Result<Self, #err_type> },
            quote! {
                match ipc::parse_reply_envelope(retbuffer, len, server.get()) {
                    Ok(__off) => {
                        #parse_handle
                        Ok(#make_self)
                    }
                    Err(e) => Err(#err_type::from_wire(e)),
                }
            },
        ),
        ConstructorReturn::Result(error_type) => (
            quote! { core::result::Result<core::result::Result<Self, #error_type>, #err_type> },
            quote! {
                match ipc::parse_reply_envelope(retbuffer, len, server.get()) {
                    Ok(__off) => {
                        let Some(&__tag) = retbuffer.get(__off) else {
                            #_p!("ipc: ctor reply truncated");
                        };
                        match __tag {
                            0u8 => {
                                let __off = __off + 1;
                                #parse_handle
                                Ok(Ok(#make_self))
                            }
                            1u8 => {
                                let Some(__err_slice) = retbuffer.get(__off + 1..len) else {
                                    #_p!("ipc: ctor domain error truncated");
                                };
                                let Ok((e, _)) =
                                    ipc::__postcard::take_from_bytes::<#error_type>(__err_slice)
                                else {
                                    #_p!("ipc: ctor domain error decode failed");
                                };
                                Ok(Err(e))
                            }
                            _ => #_p!("ipc: ctor invalid inner tag"),
                        }
                    }
                    Err(e) => Err(#err_type::from_wire(e)),
                }
            },
        ),
        ConstructorReturn::OptionSelf => (
            quote! { core::result::Result<core::option::Option<Self>, #err_type> },
            quote! {
                match ipc::parse_reply_envelope(retbuffer, len, server.get()) {
                    Ok(__off) => {
                        let Some(&__tag) = retbuffer.get(__off) else {
                            #_p!("ipc: ctor reply truncated");
                        };
                        match __tag {
                            0u8 => {
                                let __off = __off + 1;
                                #parse_handle
                                Ok(Some(#make_self))
                            }
                            1u8 => Ok(None),
                            _ => #_p!("ipc: ctor invalid option tag"),
                        }
                    }
                    Err(e) => Err(#err_type::from_wire(e)),
                }
            },
        ),
    }
}
