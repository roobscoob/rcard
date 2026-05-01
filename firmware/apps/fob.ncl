## rcard — fob application

let lib = import "../lib.ncl" in

fun args =>
  {
    name = "Charm Production",
    version = "0.2.0",
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
    boot = {
      ftab = args.places.boot,
      image = args.places.firmware,
    },

    notifications = {
      logs         = { min_priority = 0,  max_priority = 10 },
      usb_event    = { min_priority = 15, max_priority = 15 },
      usart_event  = { min_priority = 15, max_priority = 15 },
      host_request = { min_priority = 20, max_priority = 20 },
      present      = { min_priority = 25, max_priority = 25 },
    },

    trusted_senders = [
      # See stub.ncl for rationale.
      args.tasks.sysmodule_host_proxy,
    ],

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

      ## You can think of the following as 'host dependencies'

      # Enable the host proxy, so the host can drive the stub firmware.
      args.tasks.sysmodule_host_proxy,

      # Include the USB protocol sysmodules so that the host can talk
      # to the stub firmware over USB, and so that the stub firmware
      # can talk to the host over USB.
      args.tasks.sysmodule_usb_protocol,
      args.tasks.sysmodule_usb_protocol_host,
      args.tasks.sysmodule_usb_protocol_fob,

      # Include the USB log sink sysmodule so that the host can receive
      # logs from the stub firmware over USB.
      args.tasks.sysmodule_log_sink_usb,

      # We want to include the storage sysmodule in the stub firmware so
      # that we can flash the main firmware to the device
      args.tasks.sysmodule_storage,
    ],
  }
