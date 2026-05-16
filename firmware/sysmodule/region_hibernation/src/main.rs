#![no_std]
#![no_main]

use generated::slots::SLOTS;
use rcard_log::{error, info};
use sysmodule_region_hibernation_api::*;

sysmodule_log_api::bind_log!(Log = SLOTS.sysmodule_log);
rcard_log::bind_logger!(Log);
sysmodule_log_api::panic_handler!(Log);

const OP_HIBERNATE_REGION: u16 = 0x4801;
const OP_RESTORE_REGION: u16 = 0x4802;
const OP_READ_HIBERNATED: u16 = 0x4803;
const OP_WRITE_HIBERNATED: u16 = 0x4804;

const CHUNK_SIZE: usize = 240;

fn send_to_supervisor(
    op: u16,
    buf: &mut [u8],
    arg_len: usize,
    leases: &mut [userlib::Lease<'_>],
) -> (userlib::ResponseCode, usize) {
    userlib::sys_send(SLOTS.supervisor, op, buf, arg_len, leases)
        .expect("supervisor died")
}

fn supervisor_hibernate(base: u32, size: u32) -> Result<u32, HibernateError> {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&base.to_le_bytes());
    buf[4..8].copy_from_slice(&size.to_le_bytes());

    let (rc, len) = send_to_supervisor(
        OP_HIBERNATE_REGION,
        &mut buf,
        8,
        &mut [],
    );
    if rc != userlib::ResponseCode::SUCCESS {
        return Err(match rc.0 {
            1 => HibernateError::NoMatchingRegion,
            2 => HibernateError::AlreadyHibernated,
            3 => HibernateError::GenerationOverflow,
            4 => HibernateError::ProtectedMemory,
            _ => HibernateError::SupervisorRejected,
        });
    }
    if len < 4 {
        return Err(HibernateError::SupervisorRejected);
    }
    Ok(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))
}

fn supervisor_restore(event_id: u32, contents_lost: bool) -> Result<(), RestoreError> {
    let mode: u32 = if contents_lost { 1 } else { 0 };
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&event_id.to_le_bytes());
    buf[4..8].copy_from_slice(&mode.to_le_bytes());

    let (rc, _) = send_to_supervisor(
        OP_RESTORE_REGION,
        &mut buf,
        8,
        &mut [],
    );
    if rc != userlib::ResponseCode::SUCCESS {
        return Err(match rc.0 {
            1 => RestoreError::InvalidToken,
            _ => RestoreError::SupervisorRejected,
        });
    }
    Ok(())
}

fn supervisor_read(
    address: u32,
    len: u32,
    dest: &mut [u8],
) -> Result<usize, RegionIoError> {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&address.to_le_bytes());
    buf[4..8].copy_from_slice(&len.to_le_bytes());

    let mut leases = [userlib::Lease::write_only(dest)];
    let (rc, copied) = send_to_supervisor(
        OP_READ_HIBERNATED,
        &mut buf,
        8,
        &mut leases,
    );
    if rc != userlib::ResponseCode::SUCCESS {
        return Err(match rc.0 {
            1 => RegionIoError::NotHibernated,
            _ => RegionIoError::SupervisorRejected,
        });
    }
    Ok(copied)
}

fn supervisor_write(
    address: u32,
    src: &[u8],
) -> Result<usize, RegionIoError> {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&address.to_le_bytes());
    buf[4..8].copy_from_slice(&(src.len() as u32).to_le_bytes());

    let mut leases = [userlib::Lease::read_only(src)];
    let (rc, copied) = send_to_supervisor(
        OP_WRITE_HIBERNATED,
        &mut buf,
        8,
        &mut leases,
    );
    if rc != userlib::ResponseCode::SUCCESS {
        return Err(match rc.0 {
            1 => RegionIoError::NotHibernated,
            _ => RegionIoError::SupervisorRejected,
        });
    }
    Ok(copied)
}

struct HibernationGuard {
    event_id: u32,
    restored: bool,
}

impl RegionHibernation for HibernationGuard {
    fn hibernate(
        _meta: ipc::Meta,
        base: u32,
        size: u32,
    ) -> Result<Self, HibernateError> {
        info!("hibernate: base={} size={}", base, size);
        let event_id = supervisor_hibernate(base, size)?;
        Ok(HibernationGuard {
            event_id,
            restored: false,
        })
    }

    fn restore(&mut self, _meta: ipc::Meta) -> Result<(), RestoreError> {
        if self.restored {
            return Ok(());
        }
        info!("restore: event_id={}", self.event_id);
        supervisor_restore(self.event_id, false)?;
        self.restored = true;
        Ok(())
    }

    fn read(
        &mut self,
        _meta: ipc::Meta,
        address: u32,
        buf: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Write>,
    ) -> Result<u32, RegionIoError> {
        let mut scratch = [0u8; CHUNK_SIZE];
        let total = buf.len();
        let mut done = 0usize;
        while done < total {
            let chunk = (total - done).min(CHUNK_SIZE);
            let n = supervisor_read(
                address + done as u32,
                chunk as u32,
                &mut scratch[..chunk],
            )?;
            buf.write_range(done, &scratch[..n])
                .ok_or(RegionIoError::SupervisorRejected)?;
            done += n;
            if n < chunk {
                break;
            }
        }
        Ok(done as u32)
    }

    fn write(
        &mut self,
        _meta: ipc::Meta,
        address: u32,
        data: ipc::dispatch::LeaseBorrow<'_, ipc::dispatch::Read>,
    ) -> Result<u32, RegionIoError> {
        let mut scratch = [0u8; CHUNK_SIZE];
        let total = data.len();
        let mut done = 0usize;
        while done < total {
            let chunk = (total - done).min(CHUNK_SIZE);
            data.read_range(done, &mut scratch[..chunk])
                .ok_or(RegionIoError::SupervisorRejected)?;
            let n = supervisor_write(
                address + done as u32,
                &scratch[..chunk],
            )?;
            done += n;
            if n < chunk {
                break;
            }
        }
        Ok(done as u32)
    }
}

impl Drop for HibernationGuard {
    fn drop(&mut self) {
        if !self.restored {
            error!("guard dropped without restore, marking contents lost");
            let _ = supervisor_restore(self.event_id, true);
        }
    }
}

#[export_name = "main"]
fn main() -> ! {
    info!("Awake");

    ipc::server! {
        RegionHibernation: HibernationGuard,
    }
}
