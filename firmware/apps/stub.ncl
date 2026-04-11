## rcard — stub application (for flashing)

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
      logs         = { min_priority = 0,  max_priority = 10 },
      usb_event    = { min_priority = 15, max_priority = 15 },
      usart_event  = { min_priority = 15, max_priority = 15 },
      host_request = { min_priority = 20, max_priority = 20 },
      present      = { min_priority = 25, max_priority = 25 },
    },

    trusted_senders = [
      # `host_proxy` is the tunneled-IPC dispatcher. It needs to reach
      # arbitrary target servers on behalf of the host, so it sits
      # universally trusted rather than listing every target in its
      # `depends_on`. In-degree-zero (nothing sends TO host_proxy) is
      # enforced structurally by the absence of an api crate.
      args.tasks.sysmodule_host_proxy,
    ],

    filesystems = {},

    entries = [
      args.tasks.supervisor,
      args.tasks.idle,
      args.tasks.stub,
    ],
  }
