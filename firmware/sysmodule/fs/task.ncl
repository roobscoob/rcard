let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_mpi,
    ctx.tasks.sysmodule_storage,
    ctx.tasks.sysmodule_log,
  ],
  regions = {
    code  = { place = ctx.places.image },
    stack = { place = ctx.places.task_ram, size = lib.kiB 32 },
    data  = { place = ctx.places.task_ram },
  },
}
