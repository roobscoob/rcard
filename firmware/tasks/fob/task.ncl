let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.app,
  depends_on = [
    ctx.tasks.sysmodule_bluetooth,
    ctx.tasks.sysmodule_cap1208,
    ctx.tasks.sysmodule_compositor,
    ctx.tasks.sysmodule_drv2603,
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_reactor,
  ],
  uses_notifications = ["present"],
  regions = {
    code  = { place = ctx.places.code_generic, execute = true },
    stack = { place = ctx.places.fast, size = lib.kiB 8 },
    data  = { place = ctx.places.fast },
  },
}
