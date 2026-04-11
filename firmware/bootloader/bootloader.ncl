let lib = import "../lib.ncl" in

fun places => {
  crate_info = import "./Cargo.toml",
  regions = {
    code  = { place = places.bootloader },
    stack = { place = places.kernel_ram, size = 256 },
  },
}
