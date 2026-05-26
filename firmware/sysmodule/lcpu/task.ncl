let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  uses_peripherals = [
    # MAILBOX1 + HPSYS_AON aggregated into one MPU region — see chip.ncl
    # comment. We're at the 7-region effective budget (8 minus Hubris's
    # null sentinel)
    "hpsys_ipc",
    "lpsys",
    "lpsys_sram",
    "hcpu_tx",
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
    ctx.tasks.sysmodule_device,
    # rf_cal reads factory Bank1 calibration (edr_cal_done, pa_bm, etc.)
    # via the efuse IPC's read_calibration() method.
    ctx.tasks.sysmodule_efuse,
  ],
  pushes_notifications = ["lcpu_data"],
  regions = {
    code    = { place = ctx.places.code_generic, execute = true },
    # TODO: optimize stack
    stack   = { place = ctx.places.fast, size = lib.kiB 8 },
    data    = { place = ctx.places.fast },
  },
}
