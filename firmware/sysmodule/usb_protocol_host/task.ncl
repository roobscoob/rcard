let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_reactor,
    ctx.tasks.sysmodule_usb,
    ctx.tasks.sysmodule_usb_protocol,
  ],
  uses_notifications = ["usb_event"],
  pushes_notifications = ["host_request"],
  # `host_proxy` is referenced purely for visibility — the transport
  # checks `PEERS.sysmodule_host_proxy.is_some()` at codegen time to
  # decide whether to NACK incoming IpcRequests with `NoHostForwarding`.
  # peers grant no ACL, so this doesn't open a sending edge in either
  # direction.
  peers = ["sysmodule_host_proxy"],
  regions = {
    code    = { place = ctx.places.bulk },
    stack   = { place = ctx.places.fast, size = 1536 },
    data    = { place = ctx.places.fast },
    tunnel  = { shared = "tunnel" },
  },
}
