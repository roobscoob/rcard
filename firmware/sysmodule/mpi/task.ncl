let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  uses_peripherals = ["mpi1", "mpi2", "pmuc", "hpsys_rcc"],
  depends_on = [
    ctx.tasks.sysmodule_log,
  ],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = 2048 },
    data  = { place = ctx.places.fast },
  },
}
