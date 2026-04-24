let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.core_sysmodule,
  depends_on = [
    ctx.tasks.sysmodule_log,
    ctx.tasks.sysmodule_reactor,
    # Transport is NOT listed here — host_proxy reaches it through
    # `peers` + the app-level `trusted_senders` ACL exception. Listing
    # it in `depends_on` would create a normal IPC edge that a tunneled
    # request could exploit to re-enter the transport.
  ],
  peers = [
    "sysmodule_usb_protocol_host",
    # sysmodule_log is also a HostTransport — it implements the same trait
    # over its USART2 wire. host_proxy dispatches tunneled requests from
    # whichever transport pushed the notification.
    "sysmodule_log",
  ],
  uses_notifications = ["host_request"],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = lib.kiB 16 },
    data  = { place = ctx.places.fast },
  },
}
