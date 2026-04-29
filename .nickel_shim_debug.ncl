let _board = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/boards/bentoboard.ncl" in
  let _layout = (import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/layouts/ramboot.ncl") _board.memory in
  let task_fns = {
    fob = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/tasks/fob/task.ncl",
    idle = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/tasks/idle/task.ncl",
    supervisor = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/tasks/supervisor/task.ncl",
    sysmodule_compositor = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/compositor/task.ncl",
    sysmodule_device = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/device/task.ncl",
    sysmodule_display = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/display/task.ncl",
    sysmodule_efuse = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/efuse/task.ncl",
    sysmodule_fs = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/fs/task.ncl",
    sysmodule_host_proxy = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/host_proxy/task.ncl",
    sysmodule_log = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/log/task.ncl",
    sysmodule_log_sink_storage = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/log_sink_storage/task.ncl",
    sysmodule_log_sink_usb = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/log_sink_usb/task.ncl",
    sysmodule_mpi = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/mpi/task.ncl",
    sysmodule_rand = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/rand/task.ncl",
    sysmodule_reactor = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/reactor/task.ncl",
    sysmodule_storage = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/storage/task.ncl",
    sysmodule_time = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/time/task.ncl",
    sysmodule_usart = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/usart/task.ncl",
    sysmodule_usb = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/usb/task.ncl",
    sysmodule_usb_protocol = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/usb_protocol/task.ncl",
    sysmodule_usb_protocol_fob = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/usb_protocol_fob/task.ncl",
    sysmodule_usb_protocol_host = import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/sysmodule/usb_protocol_host/task.ncl"
  } in
  let rec ctx = {
    tasks = std.record.map (fun _name f => f ctx) task_fns,
    places = _layout,
  } in
  (import "C:/Users/Rose Kodsi-Hall/Documents/projects/rcard/firmware/apps/stub.ncl") { board = _board, tasks = ctx.tasks, places = _layout }