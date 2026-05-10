let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_reactor,
    ctx.tasks.sysmodule_clocks,
  ],
  uses_peripherals = ["pinctrl", "hpsys_cfg", "usbc"],
  pushes_notifications = ["usb_event"],
  regions = {
    code  = { place = ctx.places.code_generic, execute = true },
    stack = { place = ctx.places.fast, size = 3072 },
    data  = { place = ctx.places.fast },
  },
}
