let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.core_sysmodule,
  depends_on = [ctx.tasks.supervisor, ctx.tasks.sysmodule_log],
  regions = {
    # RAM-resident — this sysmodule must remain runnable while flash
    # regions are hibernated. Never place in code_generic.
    code  = { place = ctx.places.bulk, execute = true },
    stack = { place = ctx.places.fast, size = lib.kiB 2 },
    data  = { place = ctx.places.fast },
  },
}
