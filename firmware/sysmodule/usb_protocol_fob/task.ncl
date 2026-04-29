let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_reactor,
    ctx.tasks.sysmodule_usb,
    ctx.tasks.sysmodule_usb_protocol,
  ],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = 2048 },
    data  = { place = ctx.places.fast },
  },
}
