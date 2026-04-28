let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.core_sysmodule,
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = 1024 },
    data  = { place = ctx.places.fast },
  },
}
