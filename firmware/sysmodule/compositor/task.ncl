let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_display,
    ctx.tasks.sysmodule_log,
  ],
  uses_notifications = ["present"],
  regions = {
    code          = { place = ctx.places.bulk },
    stack         = { place = ctx.places.fast, size = lib.kiB 8 },
    data          = { place = ctx.places.fast },
    frame_buffers = { place = ctx.places.psram, size = lib.kiB 512, align = 32 },
  },
}
