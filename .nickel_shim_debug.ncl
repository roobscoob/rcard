let _board = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/boards/bentoboard.ncl" in
  let _layout = (import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/layouts/prod.ncl") _board.memory in
  let task_fns = {
    fob = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/tasks/fob/task.ncl",
    idle = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/tasks/idle/task.ncl",
    supervisor = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/tasks/supervisor/task.ncl",
    sysmodule_compositor = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/compositor/task.ncl",
    sysmodule_device = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/device/task.ncl",
    sysmodule_display = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/display/task.ncl",
    sysmodule_efuse = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/efuse/task.ncl",
    sysmodule_fs = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/fs/task.ncl",
    sysmodule_host_proxy = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/host_proxy/task.ncl",
    sysmodule_lcpu = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/lcpu/task.ncl",
    sysmodule_log = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/log/task.ncl",
    sysmodule_log_sink_storage = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/log_sink_storage/task.ncl",
    sysmodule_log_sink_usb = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/log_sink_usb/task.ncl",
    sysmodule_mpi = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/mpi/task.ncl",
    sysmodule_rand = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/rand/task.ncl",
    sysmodule_reactor = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/reactor/task.ncl",
    sysmodule_storage = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/storage/task.ncl",
    sysmodule_time = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/time/task.ncl",
    sysmodule_usart = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/usart/task.ncl",
    sysmodule_usb = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/usb/task.ncl",
    sysmodule_usb_protocol = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/usb_protocol/task.ncl",
    sysmodule_usb_protocol_fob = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/usb_protocol_fob/task.ncl",
    sysmodule_usb_protocol_host = import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/sysmodule/usb_protocol_host/task.ncl"
  } in
  let rec ctx = {
    tasks = std.record.map (fun _name f => f ctx) task_fns,
    places = _layout,
  } in
  (import "C:/Users/waltermin/projects/rc_charm/software_2/firmware/apps/fob.ncl") { board = _board, tasks = ctx.tasks, places = _layout }