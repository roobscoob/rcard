let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.core_sysmodule,
  depends_on = [ctx.tasks.sysmodule_log],
  uses_peripherals = ["pmuc_syscon"],
  regions = {
    code  = { place = ctx.places.code_generic, execute = true },
    stack = { place = ctx.places.fast, size = lib.kiB 2 },
    data  = { place = ctx.places.fast },
  },
}
