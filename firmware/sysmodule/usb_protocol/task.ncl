let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_usb,
    ctx.tasks.sysmodule_device_info,
  ],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = lib.kiB 4 },
    data  = { place = ctx.places.fast },
  },
}
