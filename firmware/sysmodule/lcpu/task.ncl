let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  uses_peripherals = [
    "hpsys_aon",
    "lpsys_sram",
    "mailbox1",

    ## all of this shit got kitchen sinked
    "mailbox2",
    # "lpsys_rcc",
    # "patch",
    # "ptc2",
    # "bt_rfc",
    # "lpsys_aon",
  ],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = lib.kiB 20 },
    data  = { place = ctx.places.fast },
  },
}
