let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.app,
  depends_on = [
    # Direct LCPU control while we debug the BLE host stack — the
    # bluetooth sysmodule is bypassed.
    ctx.tasks.sysmodule_lcpu,
    ctx.tasks.sysmodule_mpr121,
    ctx.tasks.sysmodule_compositor,
    ctx.tasks.sysmodule_drv2603,
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_power,
    ctx.tasks.sysmodule_reactor,
  ],
  uses_notifications = ["present", "lcpu_data"],
  regions = {
    code  = { place = ctx.places.code_generic, execute = true },
    stack = { place = ctx.places.fast, size = lib.kiB 8 },
    data  = { place = ctx.places.fast },
  },
}
