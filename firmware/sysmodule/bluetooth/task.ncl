let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  uses_peripherals = [
    # HCI data arrives via MAILBOX2_CH1 IRQ
    # "lpsys_mailbox2_ch1",
    # TODO: IRQ may be owned by sysmodule_lcpu — use reactor notifications instead
  ],
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_reactor,
    ctx.tasks.sysmodule_lcpu,
  ],
  uses_notifications = ["lcpu_data"],
  regions = {
    code  = { place = ctx.places.bulk },
    # trouble futures are large — 16 KiB stack
    stack = { place = ctx.places.fast, size = lib.kiB 16 },
    data  = { place = ctx.places.bulk },
  },
}
