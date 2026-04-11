let lib = import "../../lib.ncl" in

fun ctx => {
  crate_info = import "./Cargo.toml",
  priority = lib.workgroup.idle,
  features = ["insomniac"],
  regions = {
    code  = { place = ctx.places.image },
    stack = { place = ctx.places.task_ram, size = 256 },
    data  = { place = ctx.places.task_ram },
  },
}
