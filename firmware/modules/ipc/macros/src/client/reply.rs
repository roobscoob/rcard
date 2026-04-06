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
    let _p = panic_path();
    let rc_check = quote! {
        if rc == ipc::ACCESS_VIOLATION {
            #_p!(
                "ipc: server {:?} rejected our message: access violation \
                 (this task is not authorized to use this server)",
                #server_expr,
            );
        }
        if rc != ipc::kern::ResponseCode::SUCCESS {
            #_p!(
                "ipc: server {:?} sent unexpected non-SUCCESS response code; \
                 this indicates a protocol violation",
                #server_expr,
            );
        }
    };

    if let Some(rt) = return_type {
        let decode = wire_format::gen_decode_return_value(rt, &server_expr);
        quote! {
            #rc_check
            if len == 0 {
                #_p!("ipc: server {:?} sent empty reply", #server_expr);
            }
            let mut __off = 0usize;
            match retbuffer[0] {
                0u8 => {
                    __off += 1;
                    let __decoded = #decode;
                    Ok(__decoded)
                }
                1u8 => {
                    __off += 1;
                    let Some((err, _)) = ipc::wire::read::<ipc::Error>(&retbuffer[__off..len]) else {
                        #_p!(
                            "ipc: server {:?} sent malformed error reply ({} bytes received)",
                            #server_expr, len,
                        );
                    };
                    Err(err)
                }
                tag => #_p!(
                    "ipc: server {:?} sent invalid result tag {}",
                    #server_expr, tag,
                ),
            }
        }
    } else {
        quote! {
            #rc_check
            if len == 0 {
                #_p!("ipc: server {:?} sent empty reply", #server_expr);
            }
            match retbuffer[0] {
                0u8 => Ok(()),
                1u8 => {
                    let Some((err, _)) = ipc::wire::read::<ipc::Error>(&retbuffer[1..len]) else {
                        #_p!(
                            "ipc: server {:?} sent malformed error reply ({} bytes received)",
                            #server_expr, len,
                        );
                    };
                    Err(err)
                }
                tag => #_p!(
                    "ipc: server {:?} sent invalid result tag {}",
                    #server_expr, tag,
                ),
            }
        }
    }
}

/// Retbuffer size for a constructor call.
pub fn ctor_retbuffer_size(_ctor_return: &ConstructorReturn) -> TokenStream2 {
    quote! { ipc::HUBRIS_MESSAGE_SIZE_LIMIT }
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

    match ctor_return {
        ConstructorReturn::Bare => (
            quote! { core::result::Result<Self, #err_type> },
            quote! {
                if len == 0 { #_p!("ipc: server {:?} sent empty ctor reply", server.get()); }
                match retbuffer[0] {
                    0u8 => {
                        let Some((handle, _)) = ipc::wire::read::<ipc::RawHandle>(&retbuffer[1..len]) else {
                            #_p!("ipc: server {:?} sent malformed ctor reply ({} bytes)", server.get(), len);
                        };
                        Ok(#make_self)
                    }
                    1u8 => {
                        let Some((err, _)) = ipc::wire::read::<ipc::Error>(&retbuffer[1..len]) else {
                            #_p!("ipc: server {:?} sent malformed ctor error ({} bytes)", server.get(), len);
                        };
                        Err(#err_type::from_wire(err))
                    }
                    tag => #_p!("ipc: server {:?} sent invalid ctor tag {}", server.get(), tag),
                }
            },
        ),
        ConstructorReturn::Result(error_type) => (
            quote! { core::result::Result<core::result::Result<Self, #error_type>, #err_type> },
            quote! {
                if len == 0 { #_p!("ipc: server {:?} sent empty ctor reply", server.get()); }
                match retbuffer[0] {
                    0u8 => {
                        if len < 2 { #_p!("ipc: server {:?} sent truncated ctor reply", server.get()); }
                        match retbuffer[1] {
                            0u8 => {
                                let Some((handle, _)) = ipc::wire::read::<ipc::RawHandle>(&retbuffer[2..len]) else {
                                    #_p!("ipc: server {:?} sent malformed ctor reply ({} bytes)", server.get(), len);
                                };
                                Ok(Ok(#make_self))
                            }
                            1u8 => {
                                let Some((e, _)) = ipc::wire::read::<#error_type>(&retbuffer[2..len]) else {
                                    #_p!("ipc: server {:?} sent malformed ctor domain error ({} bytes)", server.get(), len);
                                };
                                Ok(Err(e))
                            }
                            tag => #_p!("ipc: server {:?} sent invalid inner ctor tag {}", server.get(), tag),
                        }
                    }
                    1u8 => {
                        let Some((err, _)) = ipc::wire::read::<ipc::Error>(&retbuffer[1..len]) else {
                            #_p!("ipc: server {:?} sent malformed ctor error ({} bytes)", server.get(), len);
                        };
                        Err(#err_type::from_wire(err))
                    }
                    tag => #_p!("ipc: server {:?} sent invalid ctor tag {}", server.get(), tag),
                }
            },
        ),
        ConstructorReturn::OptionSelf => (
            quote! { core::result::Result<core::option::Option<Self>, #err_type> },
            quote! {
                if len == 0 { #_p!("ipc: server {:?} sent empty ctor reply", server.get()); }
                match retbuffer[0] {
                    0u8 => {
                        if len < 2 { #_p!("ipc: server {:?} sent truncated ctor reply", server.get()); }
                        match retbuffer[1] {
                            0u8 => {
                                let Some((handle, _)) = ipc::wire::read::<ipc::RawHandle>(&retbuffer[2..len]) else {
                                    #_p!("ipc: server {:?} sent malformed ctor reply ({} bytes)", server.get(), len);
                                };
                                Ok(Some(#make_self))
                            }
                            1u8 => Ok(None),
                            tag => #_p!("ipc: server {:?} sent invalid option tag {}", server.get(), tag),
                        }
                    }
                    1u8 => {
                        let Some((err, _)) = ipc::wire::read::<ipc::Error>(&retbuffer[1..len]) else {
                            #_p!("ipc: server {:?} sent malformed ctor error ({} bytes)", server.get(), len);
                        };
                        Err(#err_type::from_wire(err))
                    }
                    tag => #_p!("ipc: server {:?} sent invalid ctor tag {}", server.get(), tag),
                }
            },
        ),
    }
}
