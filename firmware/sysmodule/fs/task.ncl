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
    code  = { place = ctx.places.code_generic, execute = true },
    stack = { place = ctx.places.fast, size = lib.kiB 32 },
    data  = { place = ctx.places.fast },
  },
}
