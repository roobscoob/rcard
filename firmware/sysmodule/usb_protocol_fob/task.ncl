let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_usb,
    ctx.tasks.sysmodule_usb_protocol,
  ],
  regions = {
    code  = { place = ctx.places.image },
    stack = { place = ctx.places.task_ram, size = lib.kiB 4 },
    data  = { place = ctx.places.task_ram },
  },
}
