use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};

use crate::parse::{HandleMode, ParsedParam};

/// Generate 2PC prepare/cancel statements for `#[handle(move)]` params and
/// clone statements for `#[handle(clone)]` params on the client side.
///
/// For move params, sends `PREPARE_TRANSFER_METHOD` to the source server.
/// If any prepare fails, sends `CANCEL_TRANSFER_METHOD` for previously
/// prepared handles and returns an error.
///
/// `server_expr` is the expression for the target server's TaskId.
pub fn gen_handle_transfer_stmts(
    params: &[&ParsedParam],
    server_expr: &TokenStream2,
    err_type: &TokenStream2,
) -> TokenStream2 {
    let move_params: Vec<(&ParsedParam, usize)> = params
        .iter()
        .enumerate()
        .filter(|(_, p)| p.handle_mode == Some(HandleMode::Move))
        .map(|(i, p)| (*p, i))
        .collect();

    let clone_params: Vec<&ParsedParam> = params
        .iter()
        .filter(|p| p.handle_mode == Some(HandleMode::Clone))
        .copied()
        .collect();

    let mut stmts = Vec::new();

    // Clone params: atomic, non-destructive
    for p in &clone_params {
        let pname = &p.name;
        if p.impl_trait_name.is_some() {
            stmts.push(quote! {
                let #pname: ipc::DynHandle = ipc::Cloneable::clone_for(
                    #pname,
                    #server_expr,
                ).map_err(|e| #err_type::CloneFailed(stringify!(#pname), e))?;
            });
        } else {
            stmts.push(quote! {
                let __dh = ipc::Cloneable::clone_for(
                    #pname,
                    #server_expr,
                ).map_err(|e| #err_type::CloneFailed(stringify!(#pname), e))?;
                let #pname: ipc::RawHandle = __dh.handle;
            });
        }
    }

    // Move params: 2PC prepare_transfer
    for (idx, (p, _)) in move_params.iter().enumerate() {
        let pname = &p.name;
        let dh_var = format_ident!("__dh_{}", pname);

        // Build cancel stmts for all previously prepared handles (rollback)
        let cancel_stmts: Vec<TokenStream2> = move_params[..idx]
            .iter()
            .map(|(prev_p, _)| {
                let prev_name = &prev_p.name;
                let prev_dh = format_ident!("__dh_{}", prev_name);
                quote! {
                    {
                        let __cancel_h = #prev_dh.handle;
                        let __cancel_buf = unsafe { &mut *ipc::ipc_buf() };
                        if let Ok(s) = ipc::__postcard::to_slice(
                            &__cancel_h,
                            &mut __cancel_buf[..],
                        ) {
                            let __cancel_n = s.len();
                            let __cancel_k = #prev_dh.kind;
                            let __cancel_opcode = ipc::opcode(__cancel_k, ipc::CANCEL_TRANSFER_METHOD);
                            let mut __cancel_leases = [];
                            let _ = ipc::kern::sys_send(
                                #prev_dh.task_id(),
                                __cancel_opcode,
                                __cancel_buf,
                                __cancel_n,
                                &mut __cancel_leases,
                            );
                        }
                    }
                }
            })
            .collect();

        stmts.push(quote! {
            // Extract DynHandle info from the Transferable (consumes it, forgets Drop)
            let #dh_var: ipc::DynHandle = {
                let dh = ipc::Transferable::transfer_info(&#pname);
                core::mem::forget(#pname);
                dh
            };
            // Send PREPARE_TRANSFER to source server
            {
                let __prep_h = #dh_var.handle;
                let __target_idx: u16 = #server_expr.task_index();
                let __prep_buf = unsafe { &mut *ipc::ipc_buf() };
                let __prep_ok = if let Ok(s) = ipc::__postcard::to_slice(
                    &(__prep_h, __target_idx),
                    &mut __prep_buf[..],
                ) {
                    let __prep_n = s.len();
                    let __prep_k = #dh_var.kind;
                    let __prep_opcode = ipc::opcode(__prep_k, ipc::PREPARE_TRANSFER_METHOD);
                    let mut __prep_leases = [];
                    match ipc::kern::sys_send(
                        #dh_var.task_id(),
                        __prep_opcode,
                        __prep_buf,
                        __prep_n,
                        &mut __prep_leases,
                    ) {
                        Ok((__prep_rc, __prep_len)) => {
                            __prep_rc == ipc::kern::ResponseCode::SUCCESS
                                && __prep_len > 0
                                && __prep_buf[0] == 0u8
                        }
                        Err(_) => false,
                    }
                } else {
                    false
                };
                if !__prep_ok {
                    // Cancel all previously prepared handles
                    #(#cancel_stmts)*
                    return Err(#err_type::TransferLost(stringify!(#pname)));
                }
            }
        });
    }

    // After all prepares succeed, bind the wire-level names
    for (p, _) in &move_params {
        let pname = &p.name;
        let dh_var = format_ident!("__dh_{}", pname);
        if p.impl_trait_name.is_some() {
            stmts.push(quote! {
                let #pname: ipc::DynHandle = #dh_var;
            });
        } else {
            stmts.push(quote! {
                let #pname: ipc::RawHandle = #dh_var.handle;
            });
        }
    }

    quote! { #(#stmts)* }
}
