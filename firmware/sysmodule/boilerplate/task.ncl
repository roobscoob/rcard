let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_mpi,
    ctx.tasks.sysmodule_log,
  ],
  peers = ["sysmodule_fs"],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = 1536 },
    data  = { place = ctx.places.fast },
  },
}
