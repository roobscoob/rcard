let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.core_sysmodule,
  uses_peripherals = ["efusec"],
  regions = {
    code  = { place = ctx.places.image },
    stack = { place = ctx.places.task_ram, size = lib.kiB 1 },
    data  = { place = ctx.places.task_ram },
  },
}
