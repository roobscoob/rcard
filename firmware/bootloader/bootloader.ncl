let lib = import "../lib.ncl" in

fun places => {
  crate_info = import "./Cargo.toml",
  regions = {
    code  = { place = places.bulk },
    stack = { place = places.fast, size = 256 },
  },
}
