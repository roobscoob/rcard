## SF32LB52JUD6 — MPI IRQs present.

let base = import "sf32lb52.ncl" in

base
  & { peripherals.mpi1.irqs = { irq = 65 } }
  & { peripherals.mpi2.irqs = { irq = 66 } }
