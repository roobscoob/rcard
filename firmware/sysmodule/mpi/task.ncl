let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  # core_sysmodule, not sysmodule: this task brings up PSRAM (MPI1) at boot
  # before entering its IPC loop. Other sysmodules (compositor) place their
  # heap in PSRAM and access it from main(), so the kernel must run mpi to
  # completion of its synchronous bring-up before scheduling them.
  priority = lib.workgroup.core_sysmodule,
  uses_peripherals = ["mpi1", "mpi2"],
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_clocks,
    ctx.tasks.sysmodule_region_hibernation,
  ],
  regions = {
    # RAM-resident — this sysmodule hibernates MPI2's XIP region at
    # open(), so its own code must not live on that flash.
    code  = { place = ctx.places.bulk, execute = true },
    stack = { place = ctx.places.fast, size = 2048 },
    data  = { place = ctx.places.fast },
  },
}
