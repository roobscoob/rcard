let lib = import "../lib.ncl" in

fun places => {
  crate_info = import "./Cargo.toml",
  regions = {
    code  = { place = places.bulk, execute = true },
    stack = { place = places.fast, size = 256 },
  },
}
