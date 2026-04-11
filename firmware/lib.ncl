## Shared firmware vocabulary.
## Imported directly by task.ncl files and root configs.

{
  kiB = fun n => n * 1024,
  miB = fun n => n * 1024 * 1024,

  workgroup = {
    kernel = 0,
    core_sysmodule = 1,
    sysmodule = 2,
    app = 3,
    idle = 4,
  },

  ## Partition a memory device into named places.
  ## Each place inherits the device's mappings and block_size.
  partition = fun device parts =>
    (std.array.fold_left
      (fun state part =>
        let off = state.cursor in
        {
          cursor = off + part.size,
          places = std.record.insert part.name {
            size = part.size,
            offset = off,
            mappings = device.mappings,
            block_size = std.record.get_or "block_size" null device,
          } state.places,
        })
      { cursor = 0, places = {} }
      parts
    ).places,
}
