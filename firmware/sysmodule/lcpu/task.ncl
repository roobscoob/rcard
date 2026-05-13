let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  uses_peripherals = [
    # MAILBOX1 + HPSYS_AON aggregated into one MPU region — see chip.ncl
    # comment. We're at the 7-region effective budget (8 minus Hubris's
    # null sentinel) and the `hcpu_tx` data region pushed us right up to
    # the edge.
    "hpsys_ipc",
    "lpsys",
    "lpsys_sram",
    # "lpsys_rcc",
    # "patch",
    # "ptc2",
    # "bt_rfc",
    # "lpsys_aon",
    # "mailbox2",
  ],
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_reactor,
    ctx.tasks.sysmodule_syscon,
  ],
  pushes_notifications = ["lcpu_data"],
  regions = {
    code    = { place = ctx.places.code_generic, execute = true },
    # TODO: optimize stack
    stack   = { place = ctx.places.fast, size = lib.kiB 8 },
    data    = { place = ctx.places.fast },
    hcpu_tx = { place = ctx.places.hcpu_tx, size = 512, offset = 0 },
  },
}
