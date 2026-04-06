use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};

use crate::parse::{HandleMode, ParsedMethod, ParsedParam};

/// Generate acquire statements for move-handle params on the server side.
///
/// For same-server handles (`RawHandle`), acquires directly from the arena.
/// For cross-server handles (`DynHandle` / `impl Trait`), sends `ACQUIRE_METHOD` IPC.
///
/// Returns `(acquire_stmts, rollback_stmts)` token streams. Rollback is handled
/// inline within the acquire block.
pub fn gen_acquire_stmts(m: &ParsedMethod) -> (TokenStream2, TokenStream2) {
    let move_params: Vec<&ParsedParam> = m
        .params
        .iter()
        .filter(|p| p.handle_mode == Some(HandleMode::Move))
        .collect();

    if move_params.is_empty() {
        return (quote! {}, quote! {});
    }

    let mut acquire_stmts = Vec::new();
    let mut acquired_names = Vec::new();

    for (i, p) in move_params.iter().enumerate() {
        let pname = &p.name;
        let acquired_flag = format_ident!("__acquired_{}", pname);

        if p.impl_trait_name.is_some() {
            // Cross-server: send ACQUIRE_METHOD IPC to source server
            acquire_stmts.push(quote! {
                let #acquired_flag = {
                    let mut __acq_call = ipc::call::IpcCall::new(
                        #pname.task_id(), #pname.kind, ipc::ACQUIRE_METHOD,
                    );
                    let __h = #pname.handle;
                    __acq_call.push_arg(&__h);
                    match __acq_call.send_raw() {
                        Ok((rc, len, retbuf)) => {
                            rc == ipc::kern::ResponseCode::SUCCESS
                                && len > 0
                                && retbuf[0] == 0u8
                        }
                        _ => false,
                    }
                };
            });
        } else {
            // Same-server: acquire directly from our own arena
            acquire_stmts.push(quote! {
                let #acquired_flag = self.arena.acquire(#pname, self.self_task_index, __priority);
            });
        }

        acquired_names.push((pname.clone(), acquired_flag, i));
    }

    let all_acquire = quote! { #(#acquire_stmts)* };

    // Check all flags; if any failed, release all previously-acquired handles
    // and reply with an error.
    let flag_checks: Vec<TokenStream2> = acquired_names
        .iter()
        .enumerate()
        .map(|(check_idx, (_pname, flag, _idx))| {
            let rollback_stmts: Vec<TokenStream2> = acquired_names[..check_idx]
                .iter()
                .map(|(prev_name, prev_flag, _)| {
                    let prev_p = move_params.iter().find(|p| &p.name == prev_name).unwrap();
                    if prev_p.impl_trait_name.is_some() {
                        // Cross-server: we acquired ownership, so destroy it on the source
                        quote! {
                            if #prev_flag {
                                let mut __destroy_call = ipc::call::IpcCall::new(
                                    #prev_name.task_id(), #prev_name.kind, ipc::IMPLICIT_DESTROY_METHOD,
                                );
                                let __h = #prev_name.handle;
                                __destroy_call.push_arg(&__h);
                                let _ = __destroy_call.send_void();
                            }
                        }
                    } else {
                        quote! {
                            if #prev_flag {
                                let _ = self.arena.remove_owned(#prev_name, self.self_task_index);
                            }
                        }
                    }
                })
                .collect();
            quote! {
                if !#flag {
                    #(#rollback_stmts)*
                    reply.reply_ok(&[1u8, ipc::Error::TransferFailed as u8]);
                    return;
                }
            }
        })
        .collect();

    let acquire_block = quote! {
        #all_acquire
        #(#flag_checks)*
    };

    (acquire_block, quote! {})
}
