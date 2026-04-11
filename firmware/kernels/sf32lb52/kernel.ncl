let lib = import "../../lib.ncl" in

fun places => {
  crate_info = import "./Cargo.toml",
  regions = {
    code  = { place = places.image, align = 512 },
    stack = { place = places.kernel_ram, size = lib.kiB 1 },
    data  = { place = places.kernel_ram },
  },
}
