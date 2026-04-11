let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.app,
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_usb,
    ctx.tasks.sysmodule_reactor,
    ctx.tasks.sysmodule_device_info,
    # Added to regenerate the slots table for the refactored transports.
    # The stub's main.rs doesn't call into these; they just need to be in
    # the build graph so SLOTS.sysmodule_usb_protocol exists.
    # ctx.tasks.sysmodule_usb_protocol_host,
    # ctx.tasks.sysmodule_usb_protocol_fob,
    # ctx.tasks.sysmodule_host_proxy,
    # ctx.tasks.sysmodule_log_sink_usb,
  ],
  uses_notifications = ["usb_event"],
  regions = {
    code  = { place = ctx.places.image },
    stack = { place = ctx.places.task_ram, size = lib.kiB 8 },
    data  = { place = ctx.places.task_ram },
  },
}
