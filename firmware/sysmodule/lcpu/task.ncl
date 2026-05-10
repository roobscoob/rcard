let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  uses_peripherals = [
    "hpsys_aon",
    "mailbox1",
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
  ],
  regions = {
    code  = { place = ctx.places.bulk },
    # TODO: optimize stack
    stack = { place = ctx.places.fast, size = lib.kiB 8 },
    data  = { place = ctx.places.bulk },
  },
}
