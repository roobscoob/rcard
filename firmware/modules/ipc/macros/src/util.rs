use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::Ident;

/// Returns the token path for the panic macro to use in generated code.
/// Always emits `ipc::__ipc_panic` which dispatches based on the
/// `bare-panics` feature on the **consumer** crate (not ipc or the
/// proc-macro crate), giving per-task control.
pub fn panic_path() -> TokenStream2 {
    quote! { ipc::__ipc_panic }
}

/// Replace occurrences of `target_ident` in a type with `replacement`.
/// Handles `Option<FS>` → `Option<ipc::RawHandle>`, etc.
pub fn replace_ident_in_type(
    ty: &syn::Type,
    target: &Ident,
    replacement: &TokenStream2,
) -> TokenStream2 {
    match ty {
        syn::Type::Path(p) => {
            if let Some(ident) = p.path.get_ident() {
                if ident == target {
                    return replacement.clone();
                }
            }
            // Recurse into generic arguments.
            let segments: Vec<TokenStream2> = p
                .path
                .segments
                .iter()
                .map(|seg| {
                    let ident = &seg.ident;
                    match &seg.arguments {
                        syn::PathArguments::AngleBracketed(args) => {
                            let inner: Vec<TokenStream2> = args
                                .args
                                .iter()
                                .map(|arg| match arg {
                                    syn::GenericArgument::Type(inner_ty) => {
                                        replace_ident_in_type(inner_ty, target, replacement)
                                    }
                                    other => quote! { #other },
                                })
                                .collect();
                            quote! { #ident<#(#inner),*> }
                        }
                        _ => quote! { #seg },
                    }
                })
                .collect();
            quote! { #(#segments)::* }
        }
        _ => quote! { #ty },
    }
}

pub fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

pub fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(ch.to_lowercase().next().unwrap());
        } else {
            result.push(ch);
        }
    }
    result
}

pub fn to_screaming_snake_case(s: &str) -> String {
    to_snake_case(s).to_uppercase()
}

pub fn to_snake_ident(ident: &Ident) -> Ident {
    Ident::new(&to_snake_case(&ident.to_string()), ident.span())
}

/// If `ty` is `Option<T>`, return the inner `T`.
pub fn extract_option_inner(ty: &syn::Type) -> Option<&syn::Type> {
    if let syn::Type::Path(tp) = ty {
        let seg = tp.path.segments.last()?;
        if seg.ident == "Option" {
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                    return Some(inner);
                }
            }
        }
    }
    None
}

/// If `ty` is `Result<T, E>`, return `(T, E)`.
pub fn extract_result_types(ty: &syn::Type) -> Option<(&syn::Type, &syn::Type)> {
    if let syn::Type::Path(tp) = ty {
        let seg = tp.path.segments.last()?;
        if seg.ident == "Result" {
            if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                let mut iter = args.args.iter();
                if let (
                    Some(syn::GenericArgument::Type(ok_ty)),
                    Some(syn::GenericArgument::Type(err_ty)),
                ) = (iter.next(), iter.next())
                {
                    return Some((ok_ty, err_ty));
                }
            }
        }
    }
    None
}

/// Generate code to write a return value into `__reply_buf` at offset `__off`,
/// handling Option<T> and Result<T, E> with tag+payload encoding.
/// `__reply_buf` is `[MaybeUninit<u8>; N]`. Returns a TokenStream that writes
/// bytes and updates `__off`.
pub fn gen_encode_return_value(rt: &syn::Type, value_expr: TokenStream2) -> TokenStream2 {
    if extract_option_inner(rt).is_some() {
        quote! {
            match #value_expr {
                Some(ref __val) => {
                    ipc::wire::set_uninit(&mut __reply_buf, __off, 0u8); // Some
                    __off += 1;
                    __off += ipc::wire::write_uninit(&mut __reply_buf[__off..], __val);
                }
                None => {
                    ipc::wire::set_uninit(&mut __reply_buf, __off, 1u8); // None
                    __off += 1;
                }
            }
        }
    } else if let Some((ok_ty, _err_ty)) = extract_result_types(rt) {
        // Check if Ok type is () — don't write anything for unit
        let is_unit = matches!(ok_ty, syn::Type::Tuple(t) if t.elems.is_empty());
        let write_ok = if is_unit {
            quote! {}
        } else {
            quote! { __off += ipc::wire::write_uninit(&mut __reply_buf[__off..], __ok_val); }
        };
        quote! {
            match #value_expr {
                Ok(ref __ok_val) => {
                    ipc::wire::set_uninit(&mut __reply_buf, __off, 0u8); // Ok
                    __off += 1;
                    #write_ok
                }
                Err(ref __err_val) => {
                    ipc::wire::set_uninit(&mut __reply_buf, __off, 1u8); // Err
                    __off += 1;
                    __off += ipc::wire::write_uninit(&mut __reply_buf[__off..], __err_val);
                }
            }
        }
    } else {
        quote! {
            __off += ipc::wire::write_uninit(&mut __reply_buf[__off..], &#value_expr);
        }
    }
}

/// Generate code to decode a return value from `retbuffer[__off..len]`,
/// handling Option<T> and Result<T, E> with tag+payload decoding.
/// Returns a TokenStream that evaluates to the decoded value.
pub fn gen_decode_return_value(rt: &syn::Type, server_expr: &TokenStream2) -> TokenStream2 {
    let _p = panic_path();
    if let Some(inner) = extract_option_inner(rt) {
        quote! {{
            if __off >= len {
                #_p!("ipc: server {:?} sent truncated reply", #server_expr);
            }
            match retbuffer[__off] {
                0u8 => {
                    __off += 1;
                    let Some((__val, _)) = ipc::wire::read::<#inner>(&retbuffer[__off..len]) else {
                        #_p!("ipc: server {:?} sent malformed reply ({} bytes)", #server_expr, len);
                    };
                    __off += core::mem::size_of_val(&__val);
                    Some(__val)
                }
                1u8 => {
                    __off += 1;
                    None
                }
                __tag => #_p!("ipc: server {:?} sent invalid option tag {}", #server_expr, __tag),
            }
        }}
    } else if let Some((ok_ty, err_ty)) = extract_result_types(rt) {
        let is_unit = matches!(ok_ty, syn::Type::Tuple(t) if t.elems.is_empty());
        let read_ok = if is_unit {
            quote! { () }
        } else {
            quote! {{
                let Some((__val, _)) = ipc::wire::read::<#ok_ty>(&retbuffer[__off..len]) else {
                    #_p!("ipc: server {:?} sent malformed reply ({} bytes)", #server_expr, len);
                };
                __off += core::mem::size_of_val(&__val);
                __val
            }}
        };
        quote! {{
            if __off >= len {
                #_p!("ipc: server {:?} sent truncated reply", #server_expr);
            }
            match retbuffer[__off] {
                0u8 => {
                    __off += 1;
                    Ok(#read_ok)
                }
                1u8 => {
                    __off += 1;
                    let Some((__err, _)) = ipc::wire::read::<#err_ty>(&retbuffer[__off..len]) else {
                        #_p!("ipc: server {:?} sent malformed error reply ({} bytes)", #server_expr, len);
                    };
                    __off += core::mem::size_of_val(&__err);
                    Err(__err)
                }
                __tag => #_p!("ipc: server {:?} sent invalid result tag {}", #server_expr, __tag),
            }
        }}
    } else {
        quote! {{
            let Some((__val, _)) = ipc::wire::read::<#rt>(&retbuffer[__off..len]) else {
                #_p!("ipc: server {:?} sent malformed reply ({} bytes)", #server_expr, len);
            };
            __off += core::mem::size_of_val(&__val);
            __val
        }}
    }
}
