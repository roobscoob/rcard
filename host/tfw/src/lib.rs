pub mod archive;
pub mod build;
pub mod build_metadata;
pub mod codegen;
pub mod compile;
pub mod config;
pub mod elf_cache;
pub mod ipc_metadata;
pub mod layout;
pub mod link;
pub mod linker;
pub mod log_metadata;
pub mod metadata;
pub mod pack;
pub mod renode;
pub mod schema_dump;

/// Shorten an absolute path to a project-relative one.
pub fn shorten_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    for marker in ["/firmware/", "/shared/", "/modules/", "/patches/"] {
        if let Some(idx) = path.find(marker) {
            return path[idx + 1..].to_string();
        }
    }
    path
}
