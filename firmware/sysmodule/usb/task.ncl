let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_reactor,
  ],
  uses_peripherals = ["pinctrl", "rcc", "syscon", "usbc"],
  pushes_notifications = ["usb_event"],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = lib.kiB 8 },
    data  = { place = ctx.places.fast },
  },
}
