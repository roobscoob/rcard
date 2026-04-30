let _board = import "/Users/gu/Developer/rcard/firmware/boards/bentoboard.ncl" in
  let _layout = (import "/Users/gu/Developer/rcard/firmware/layouts/ramboot.ncl") _board.memory in
  let task_fns = {
    fob = import "/Users/gu/Developer/rcard/firmware/tasks/fob/task.ncl",
    idle = import "/Users/gu/Developer/rcard/firmware/tasks/idle/task.ncl",
    supervisor = import "/Users/gu/Developer/rcard/firmware/tasks/supervisor/task.ncl",
    sysmodule_boilerplate = import "/Users/gu/Developer/rcard/firmware/sysmodule/boilerplate/task.ncl",
    sysmodule_compositor = import "/Users/gu/Developer/rcard/firmware/sysmodule/compositor/task.ncl",
    sysmodule_device = import "/Users/gu/Developer/rcard/firmware/sysmodule/device/task.ncl",
    sysmodule_display = import "/Users/gu/Developer/rcard/firmware/sysmodule/display/task.ncl",
    sysmodule_efuse = import "/Users/gu/Developer/rcard/firmware/sysmodule/efuse/task.ncl",
    sysmodule_fs = import "/Users/gu/Developer/rcard/firmware/sysmodule/fs/task.ncl",
    sysmodule_host_proxy = import "/Users/gu/Developer/rcard/firmware/sysmodule/host_proxy/task.ncl",
    sysmodule_log = import "/Users/gu/Developer/rcard/firmware/sysmodule/log/task.ncl",
    sysmodule_log_sink_storage = import "/Users/gu/Developer/rcard/firmware/sysmodule/log_sink_storage/task.ncl",
    sysmodule_log_sink_usb = import "/Users/gu/Developer/rcard/firmware/sysmodule/log_sink_usb/task.ncl",
    sysmodule_mpi = import "/Users/gu/Developer/rcard/firmware/sysmodule/mpi/task.ncl",
    sysmodule_rand = import "/Users/gu/Developer/rcard/firmware/sysmodule/rand/task.ncl",
    sysmodule_reactor = import "/Users/gu/Developer/rcard/firmware/sysmodule/reactor/task.ncl",
    sysmodule_storage = import "/Users/gu/Developer/rcard/firmware/sysmodule/storage/task.ncl",
    sysmodule_time = import "/Users/gu/Developer/rcard/firmware/sysmodule/time/task.ncl",
    sysmodule_usart = import "/Users/gu/Developer/rcard/firmware/sysmodule/usart/task.ncl",
    sysmodule_usb = import "/Users/gu/Developer/rcard/firmware/sysmodule/usb/task.ncl",
    sysmodule_usb_protocol = import "/Users/gu/Developer/rcard/firmware/sysmodule/usb_protocol/task.ncl",
    sysmodule_usb_protocol_fob = import "/Users/gu/Developer/rcard/firmware/sysmodule/usb_protocol_fob/task.ncl",
    sysmodule_usb_protocol_host = import "/Users/gu/Developer/rcard/firmware/sysmodule/usb_protocol_host/task.ncl"
  } in
  let rec ctx = {
    tasks = std.record.map (fun _name f => f ctx) task_fns,
    places = _layout,
  } in
  (import "/Users/gu/Developer/rcard/firmware/apps/stub.ncl") { board = _board, tasks = ctx.tasks, places = _layout }