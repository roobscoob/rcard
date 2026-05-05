let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  uses_peripherals = ["lcdc", "gpios"],
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_clocks,
    ctx.tasks.sysmodule_syscon,
  ],
  regions = {
    code  = { place = ctx.places.code_generic, execute = true },
    stack = { place = ctx.places.fast, size = 1536 },
    data  = { place = ctx.places.fast },
  },
}
