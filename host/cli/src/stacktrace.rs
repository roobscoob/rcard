use std::collections::HashMap;

use object::read::File as ObjectFile;
use object::{Object, ObjectSection};
use rcard_log::OwnedValue;

/// A single frame in a resolved backtrace.
pub struct Frame {
    pub function: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub is_inline: bool,
}

/// Resolved backtrace produced from a stack dump + ELF debug info.
pub struct Backtrace {
    pub frames: Vec<Frame>,
}

/// Per-task ELF data for resolving addresses.
pub struct TaskElf {
    data: Vec<u8>,
}

/// Collection of loaded task ELFs, keyed by task name.
pub struct ElfCache {
    elfs: HashMap<String, TaskElf>,
}

impl ElfCache {
    pub fn new() -> Self {
        Self {
            elfs: HashMap::new(),
        }
    }

    pub fn load_from_archive(&mut self, archive: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>) {
        for i in 0..archive.len() {
            let Ok(mut entry) = archive.by_index(i) else {
                continue;
            };
            let name = entry.name().to_string();
            if !name.starts_with("elf/") {
                continue;
            }
            let task_name = match name.rsplit('/').next() {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => continue,
            };
            let mut data = Vec::new();
            if std::io::Read::read_to_end(&mut entry, &mut data).is_ok() {
                self.elfs.insert(task_name, TaskElf { data });
            }
        }
    }

    /// Resolve a stack dump into a backtrace for the given task.
    pub fn resolve(&self, task_name: &str, dump: &OwnedValue) -> Option<Backtrace> {
        let OwnedValue::StackDump {
            sp,
            stack_top: _,
            lr,
            pc,
            registers: _,
            xpsr: _,
            stack,
        } = dump
        else {
            return None;
        };

        let elf = self.elfs.get(task_name)?;
        let object: ObjectFile<&[u8]> = ObjectFile::parse(elf.data.as_slice()).ok()?;
        let endian = gimli::LittleEndian;

        let dwarf = gimli::Dwarf::load(
            |section_id| -> Result<gimli::EndianSlice<gimli::LittleEndian>, gimli::Error> {
                let data = object
                    .section_by_name(section_id.name())
                    .and_then(|s| s.data().ok())
                    .unwrap_or(&[]);
                Ok(gimli::EndianSlice::new(data, endian))
            },
        )
        .ok()?;
        let ctx = addr2line::Context::from_dwarf(dwarf).ok()?;

        // Fetch CFI unwinding sections
        use gimli::UnwindSection;
        let debug_frame_data = object
            .section_by_name(".debug_frame")
            .and_then(|s| s.data().ok())
            .unwrap_or(&[]);
        let mut debug_frame =
            gimli::DebugFrame::from(gimli::EndianSlice::new(debug_frame_data, endian));
        debug_frame.set_address_size(4); // 32-bit ARM

        let eh_frame_data = object
            .section_by_name(".eh_frame")
            .and_then(|s| s.data().ok())
            .unwrap_or(&[]);
        let eh_frame = gimli::EhFrame::from(gimli::EndianSlice::new(eh_frame_data, endian));

        let mut unwind_ctx = gimli::UnwindContext::new();
        let bases = gimli::BaseAddresses::default();

        let mut current_pc = *pc as u64;
        let mut current_sp = *sp as u64;
        let mut addresses: Vec<u64> = Vec::new();

        // Helper to read 4-byte values directly from the captured stack dump
        let read_stack = |addr: u64| -> Option<u32> {
            let offset = addr.checked_sub(*sp as u64)? as usize;
            let bytes: [u8; 4] = stack.get(offset..offset + 4)?.try_into().ok()?;
            Some(u32::from_le_bytes(bytes))
        };

        const MAX_FRAMES: usize = 64;

        // Deterministically unwind the stack using DWARF CFI
        while addresses.len() < MAX_FRAMES {
            let actual_pc = current_pc & !1; // Mask Thumb bit for accurate table lookups
            addresses.push(actual_pc);

            // Attempt to get the unwind rule for the current address
            let unwind_info = match debug_frame.unwind_info_for_address(
                &bases,
                &mut unwind_ctx,
                actual_pc,
                |section, bases, offset| section.cie_from_offset(bases, offset),
            ) {
                Ok(info) => Ok(info),
                Err(_) => eh_frame.unwind_info_for_address(
                    &bases,
                    &mut unwind_ctx,
                    actual_pc,
                    |section, bases, offset| section.cie_from_offset(bases, offset),
                ),
            };

            let Ok(unwind_info) = unwind_info else {
                break; // Stop if DWARF unwinding information is unavailable
            };

            // 1. Calculate Canonical Frame Address (CFA)
            let cfa = match unwind_info.cfa() {
                gimli::CfaRule::RegisterAndOffset { register, offset } => {
                    // ARM SP is register 13
                    if register.0 == 13 {
                        (current_sp as i64 + *offset) as u64
                    } else {
                        break; // Unsupported CFA register
                    }
                }
                _ => break, // Unsupported CFA evaluation rule
            };

            // 2. Evaluate the saved Return Address
            // ARM LR is register 14. This tells us where the caller's PC is.
            let ra_rule = unwind_info.register(gimli::Register(14));
            let next_pc = match ra_rule {
                gimli::RegisterRule::Undefined => {
                    // In the leaf frame (our faulting capture site), if LR wasn't saved to the stack,
                    // the current live `lr` register contains our return address.
                    if addresses.len() == 1 {
                        *lr as u64
                    } else {
                        break;
                    }
                }
                gimli::RegisterRule::SameValue => {
                    // SameValue means LR wasn't modified — only meaningful at the leaf
                    if addresses.len() == 1 {
                        *lr as u64
                    } else {
                        break;
                    }
                }
                gimli::RegisterRule::Offset(offset) => {
                    let addr = (cfa as i64 + offset) as u64;
                    match read_stack(addr) {
                        Some(val) => val as u64,
                        None => break,
                    }
                }
                gimli::RegisterRule::ValOffset(offset) => (cfa as i64 + offset) as u64,
                _ => break, // Unsupported RA rule
            };

            // Break on end-of-stack bounds or cyclic unwinding
            if next_pc == 0 || next_pc == current_pc {
                break;
            }

            current_pc = next_pc;
            current_sp = cfa;
        }

        addresses.dedup();
        let mut frames = Vec::new();

        for addr in &addresses {
            let Ok(mut frame_iter) = ctx.find_frames(*addr).skip_all_loads() else {
                continue;
            };

            let mut addr_frames: Vec<Frame> = Vec::new();
            while let Ok(Some(frame)) = frame_iter.next() {
                let function = frame
                    .function
                    .as_ref()
                    .and_then(|f| f.demangle().ok())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("0x{:08x}", addr));

                let (file, line) = match frame.location {
                    Some(ref loc) => {
                        let file = loc.file.map(shorten_path);
                        (file, loc.line)
                    }
                    None => (None, None),
                };

                addr_frames.push(Frame {
                    function,
                    file,
                    line,
                    is_inline: false,
                });
            }

            // All frames except the last (outermost) for this address are inlined
            let count = addr_frames.len();
            if count > 1 {
                for f in addr_frames.iter_mut().take(count - 1) {
                    f.is_inline = true;
                }
            }

            frames.extend(addr_frames);
        }

        if frames.is_empty() {
            frames.push(Frame {
                function: format!("0x{:08x}", pc),
                file: None,
                line: None,
                is_inline: false,
            });
        }

        Some(Backtrace { frames })
    }
}

/// Shorten an absolute path to a project-relative one.
fn shorten_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    for marker in ["/firmware/", "/shared/", "/modules/", "/patches/"] {
        if let Some(idx) = path.find(marker) {
            return path[idx + 1..].to_string();
        }
    }
    path
}
