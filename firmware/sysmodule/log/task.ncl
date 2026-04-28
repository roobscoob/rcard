let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.core_sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_usart,
    ctx.tasks.sysmodule_reactor,
    ctx.tasks.sysmodule_efuse,
  ],
  pushes_notifications = ["logs", "host_request"],
  uses_notifications = ["usart_event"],
  # `host_proxy` is referenced purely for visibility — the RX handler
  # checks `PEERS.sysmodule_host_proxy.is_some()` at codegen time and
  # NACKs inbound IpcRequests if the build has no dispatcher.
  peers = ["sysmodule_host_proxy"],
  regions = {
    code    = { place = ctx.places.bulk },
    stack   = { place = ctx.places.fast, size = 1536 },
    data    = { place = ctx.places.fast },
    tunnel  = { shared = "tunnel" },
  },
}
