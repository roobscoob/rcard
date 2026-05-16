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
    # RAM-resident — this sysmodule opens MPI2, which hibernates the
    # MPI2 XIP region. Code on that flash would be suspended.
    code  = { place = ctx.places.bulk, execute = true },
    stack = { place = ctx.places.fast, size = 1536 },
    data  = { place = ctx.places.fast },
  },
}
