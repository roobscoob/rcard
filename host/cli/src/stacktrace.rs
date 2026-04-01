use std::collections::HashMap;

use object::read::File as ObjectFile;
use object::{Object, ObjectSection, SectionKind};
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
            sp: _,
            stack_top: _,
            lr,
            pc: _,
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
        let dwarf = gimli::Dwarf::load(|section_id| -> Result<gimli::EndianSlice<gimli::LittleEndian>, gimli::Error> {
            let data = object
                .section_by_name(section_id.name())
                .and_then(|s| s.data().ok())
                .unwrap_or(&[]);
            Ok(gimli::EndianSlice::new(data, endian))
        }).ok()?;
        let ctx = addr2line::Context::from_dwarf(dwarf).ok()?;

        let mut frames = Vec::new();

        // Start with LR (return address from the capture site), then walk
        // return addresses found on the stack.
        let mut addresses: Vec<u64> = vec![*lr as u64];

        // Scan the stack for plausible return addresses.
        // On Cortex-M Thumb-2, return addresses have bit 0 set.
        let code_range = code_address_range(&object);
        for chunk in stack.chunks_exact(4) {
            let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            if word & 1 == 1 {
                let addr = (word & !1) as u64;
                if code_range.contains(&addr) {
                    addresses.push(addr);
                }
            }
        }

        addresses.dedup();

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
                function: format!("0x{:08x}", lr),
                file: None,
                line: None,
                is_inline: false,
            });
        }

        Some(Backtrace { frames })
    }
}

/// Find the address range of loadable code segments in the ELF.
fn code_address_range<'a>(object: &ObjectFile<'a, &'a [u8]>) -> std::ops::Range<u64> {
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    for section in object.sections() {
        if let SectionKind::Text = section.kind() {
            let start = section.address();
            let end = start + section.size();
            lo = lo.min(start);
            hi = hi.max(end);
        }
    }
    if lo > hi { 0..0 } else { lo..hi }
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
