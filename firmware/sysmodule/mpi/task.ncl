let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  # core_sysmodule, not sysmodule: this task brings up PSRAM (MPI1) at boot
  # before entering its IPC loop. Other sysmodules (compositor) place their
  # heap in PSRAM and access it from main(), so the kernel must run mpi to
  # completion of its synchronous bring-up before scheduling them.
  priority = lib.workgroup.core_sysmodule,
  uses_peripherals = ["mpi1", "mpi2", "pmuc_syscon", "rcc", "syscon"],
  depends_on = [
    ctx.tasks.sysmodule_log,
  ],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = 2048 },
    data  = { place = ctx.places.fast },
  },
}
