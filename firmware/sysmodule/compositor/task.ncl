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
    code          = { place = ctx.places.image },
    stack         = { place = ctx.places.task_ram, size = lib.kiB 8 },
    data          = { place = ctx.places.task_ram },
    frame_buffers = { place = ctx.places.framebuf, size = lib.kiB 512, align = 32 },
  },
}
