let lib = import "../../lib.ncl" in

fun places => {
  crate_info = import "./Cargo.toml",
  regions = {
    code  = { place = places.bulk, align = 512 },
    stack = { place = places.fast, size = lib.kiB 1 },
    data  = { place = places.fast },
  },
}
