let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.kernel,
  supervisor = true,
  uses_peripherals = ["usart1"],
  # Peer reference exposes `generated::peers::PEERS.sysmodule_log` at codegen
  # time so `handle_emit_log` can ACL its caller against the known
  # sysmodule_log task index. Tolerated `None` if a build excludes log.
  peers = ["sysmodule_log"],
  regions = {
    code  = { place = ctx.places.bulk },
    stack = { place = ctx.places.fast, size = lib.kiB 8 },
    data  = { place = ctx.places.fast },
  },
}
