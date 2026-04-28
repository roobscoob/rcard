let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.core_sysmodule,
  uses_peripherals = ["usart2", "usart3"],
  depends_on = [
    ctx.tasks.sysmodule_reactor,
  ],
  pushes_notifications = ["usart_event"],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = 1024 },
    data  = { place = ctx.places.fast },
  },
}
