let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.idle,
  features = ["insomniac"],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = 256 },
    data  = { place = ctx.places.fast },
  },
}
