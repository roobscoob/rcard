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
    code  = { place = ctx.places.code_generic, execute = true },
    stack = { place = ctx.places.fast, size = 2048 },
    data  = { place = ctx.places.fast },
  },
}
