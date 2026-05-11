let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_clocks,
    ctx.tasks.sysmodule_log,
  ],
  uses_peripherals = ["gptim2", "gpios"],
  regions = {
    code  = { place = ctx.places.code_generic, execute = true },
    stack = { place = ctx.places.fast, size = lib.kiB 1 },
    data  = { place = ctx.places.fast },
  },
}
