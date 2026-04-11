## rcard — fob application

let lib = import "../lib.ncl" in

fun args =>
  {
    name = "rcard",
    version = "0.1.0",
    target = args.board.target,
    vector_table_size = args.board.vector_table_size,
    kernel = (args.board.kernel) args.places,
    bootloader =
      if std.record.has_field "bootloader" args.board then
        (args.board.bootloader) args.places
      else
        null,
    places = args.places,
    memory = args.board.memory,
    peripheral_map = args.board.peripheral_map,
    pins = args.board.pins,
    pin_assignments = args.board.pin_assignments,

    boot = {
      ftab = args.places.sec_config,
      loader = args.places.bootloader,
    },

    notifications = {
      logs    = { min_priority = 0,  max_priority = 10 },
      present = { min_priority = 25, max_priority = 25 },
    },

    filesystems = {
      global = {
        mounts = [
          { name = "main", source = "fs_main" },
        ],
      },
    },

    entries = [
      args.tasks.supervisor,
      args.tasks.idle,
      args.tasks.fob,
    ],
  }
