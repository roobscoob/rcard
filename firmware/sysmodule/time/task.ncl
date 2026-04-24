let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.core_sysmodule,
  uses_peripherals = ["rtc"],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = lib.kiB 1 },
    data  = { place = ctx.places.fast },
  },
}
