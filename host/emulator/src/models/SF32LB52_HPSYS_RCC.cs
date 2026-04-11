//
// SiFli SF32LB52x HPSYS_RCC peripheral model for Renode.
// Base address: 0x50000000, size 0x48.
//
// Models the bits of the clock & reset controller that firmware actually
// spins on during boot:
//   * DLL1CR / DLL2CR  -- READY latches high one cycle after EN is written 1
//   * ENR1/2 + ESR1/2 + ECR1/2 -- standard set/clear shadow of the enable regs
//   * RSTRx, CSR, CFGR, USBCR, DBGCLKR, DBGR, DWCFGR -- plain storage
//   * HRCCAL1/2 -- CAL_DONE latches when CAL_EN goes high; counts mirror
//                  CAL_LENGTH so the firmware sees a perfectly calibrated HRC
//
// Reference: UM5201-SF32LB52x User Manual v0.8.4, section 2.8.
//
// Drop into: src/Infrastructure/src/Emulator/Peripherals/Peripherals/Miscellaneous/
//
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure.Registers;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;

namespace Antmicro.Renode.Peripherals.Miscellaneous
{
    public class SF32LB52_HPSYS_RCC : BasicDoubleWordPeripheral, IKnownSize
    {
        public SF32LB52_HPSYS_RCC(IMachine machine) : base(machine)
        {
            DefineRegisters();
        }

        public long Size => 0x48;

        public override void Reset()
        {
            base.Reset();
            enr1 = 0;
            enr2 = 0;
        }

        private void DefineRegisters()
        {
            // 0x00 RSTR1 / 0x04 RSTR2 -- module reset, plain storage. Firmware
            // toggles bits and expects the module to come out of reset; we have
            // no real modules behind these so we just remember the value.
            Registers.RSTR1.Define(this);
            Registers.RSTR2.Define(this);

            // 0x08 ENR1 -- backed by `enr1`, exposed RW.
            Registers.ENR1.Define(this)
                .WithValueField(0, 32, name: "ENR1",
                    valueProviderCallback: _ => enr1,
                    writeCallback: (_, v) => enr1 = (uint)v);

            // 0x0C ENR2
            Registers.ENR2.Define(this)
                .WithValueField(0, 32, name: "ENR2",
                    valueProviderCallback: _ => enr2,
                    writeCallback: (_, v) => enr2 = (uint)v);

            // 0x10 ESR1 -- write 1 to set the corresponding bit in ENR1.
            Registers.ESR1.Define(this)
                .WithValueField(0, 32, FieldMode.Write, name: "ESR1",
                    writeCallback: (_, v) => enr1 |= (uint)v);

            // 0x14 ESR2
            Registers.ESR2.Define(this)
                .WithValueField(0, 32, FieldMode.Write, name: "ESR2",
                    writeCallback: (_, v) => enr2 |= (uint)v);

            // 0x18 ECR1 -- write 1 to clear the corresponding bit in ENR1.
            Registers.ECR1.Define(this)
                .WithValueField(0, 32, FieldMode.Write, name: "ECR1",
                    writeCallback: (_, v) => enr1 &= ~(uint)v);

            // 0x1C ECR2
            Registers.ECR2.Define(this)
                .WithValueField(0, 32, FieldMode.Write, name: "ECR2",
                    writeCallback: (_, v) => enr2 &= ~(uint)v);

            // 0x20 CSR -- clock source mux selects. Pure storage; we don't
            // model the actual clock tree, just let firmware pick sources.
            Registers.CSR.Define(this, 0x00001000) // SEL_PERI defaults to 1
                .WithValueField(0, 2, name: "SEL_SYS")
                .WithFlag(2, name: "SEL_SYS_LP")
                .WithReservedBits(3, 1)
                .WithValueField(4, 2, name: "SEL_MPI1")
                .WithValueField(6, 2, name: "SEL_MPI2")
                .WithReservedBits(8, 4)
                .WithFlag(12, name: "SEL_PERI")
                .WithValueField(13, 2, name: "SEL_TICK")
                .WithFlag(15, name: "SEL_USBC")
                .WithReservedBits(16, 16);

            // 0x24 CFGR -- HDIV / PDIV1 / PDIV2 / TICKDIV. Storage only.
            Registers.CFGR.Define(this, 0x00120101)
                .WithValueField(0, 8, name: "HDIV")
                .WithValueField(8, 3, name: "PDIV1")
                .WithReservedBits(11, 1)
                .WithValueField(12, 3, name: "PDIV2")
                .WithReservedBits(15, 1)
                .WithValueField(16, 6, name: "TICKDIV")
                .WithReservedBits(22, 10);

            // 0x28 USBCR -- USB divider, storage.
            Registers.USBCR.Define(this, 0x4)
                .WithValueField(0, 3, name: "DIV")
                .WithReservedBits(3, 29);

            // 0x2C DLL1CR -- DLL1 control. The READY bit is the interesting
            // part: real silicon takes a few microseconds to lock, but for
            // emulation we latch it as soon as EN is written 1. Firmware
            // typically does `while(!(DLL1CR & READY));` after enabling.
            Registers.DLL1CR.Define(this, 0x0001F040)
                .WithFlag(0, name: "EN",
                    writeCallback: (_, v) => dll1Ready = v)
                .WithFlag(1, name: "SW")
                .WithValueField(2, 4, name: "STG")
                .WithFlag(6, name: "XTALIN_EN")
                .WithFlag(7, name: "MODE48M_EN")
                .WithValueField(8, 4, name: "LDO_VREF")
                .WithFlag(12, name: "IN_DIV2_EN")
                .WithFlag(13, name: "OUT_DIV2_EN")
                .WithFlag(14, name: "MCU_PRCHG_EN")
                .WithFlag(15, name: "MCU_PRCHG")
                .WithFlag(16, name: "PRCHG_EN")
                .WithFlag(17, name: "PRCHG_EXT")
                .WithFlag(18, name: "VST_SEL")
                .WithFlag(19, name: "BYPASS")
                .WithFlag(20, name: "DTEST_EN")
                .WithValueField(21, 4, name: "DTEST_TR")
                .WithValueField(25, 3, name: "PU_DLY")
                .WithValueField(28, 3, name: "LOCK_DLY")
                .WithFlag(31, FieldMode.Read, name: "READY",
                    valueProviderCallback: _ => dll1Ready);

            // 0x30 DLL2CR -- identical structure to DLL1CR.
            Registers.DLL2CR.Define(this, 0x0001F040)
                .WithFlag(0, name: "EN",
                    writeCallback: (_, v) => dll2Ready = v)
                .WithFlag(1, name: "SW")
                .WithValueField(2, 4, name: "STG")
                .WithFlag(6, name: "XTALIN_EN")
                .WithFlag(7, name: "MODE48M_EN")
                .WithValueField(8, 4, name: "LDO_VREF")
                .WithFlag(12, name: "IN_DIV2_EN")
                .WithFlag(13, name: "OUT_DIV2_EN")
                .WithFlag(14, name: "MCU_PRCHG_EN")
                .WithFlag(15, name: "MCU_PRCHG")
                .WithFlag(16, name: "PRCHG_EN")
                .WithFlag(17, name: "PRCHG_EXT")
                .WithFlag(18, name: "VST_SEL")
                .WithFlag(19, name: "BYPASS")
                .WithFlag(20, name: "DTEST_EN")
                .WithValueField(21, 4, name: "DTEST_TR")
                .WithValueField(25, 3, name: "PU_DLY")
                .WithValueField(28, 3, name: "LOCK_DLY")
                .WithFlag(31, FieldMode.Read, name: "READY",
                    valueProviderCallback: _ => dll2Ready);

            // 0x34 HRCCAL1 -- HRC calibration. CAL_DONE latches when firmware
            // sets CAL_EN. We mirror CAL_LENGTH into both counters in HRCCAL2
            // so the perceived HRC frequency exactly equals XT48.
            Registers.HRCCAL1.Define(this, 0x00008000)
                .WithValueField(0, 16, out calLength, name: "CAL_LENGTH")
                .WithReservedBits(16, 14)
                .WithFlag(30, name: "CAL_EN",
                    writeCallback: (_, v) => calDone = v)
                .WithFlag(31, FieldMode.Read, name: "CAL_DONE",
                    valueProviderCallback: _ => calDone);

            // 0x38 HRCCAL2 -- read-only counters reflecting the calibration.
            Registers.HRCCAL2.Define(this)
                .WithValueField(0, 16, FieldMode.Read, name: "HRC_CNT",
                    valueProviderCallback: _ => calDone ? calLength.Value : 0)
                .WithValueField(16, 16, FieldMode.Read, name: "HXT_CNT",
                    valueProviderCallback: _ => calDone ? calLength.Value : 0);

            // Debug-only registers below: storage is enough.
            Registers.DBGCLKR.Define(this, 0x00040400);
            Registers.DBGR.Define(this);
            Registers.DWCFGR.Define(this);
        }

        private uint enr1;
        private uint enr2;
        private bool dll1Ready;
        private bool dll2Ready;
        private bool calDone;
        private IValueRegisterField calLength;

        private enum Registers : long
        {
            RSTR1   = 0x00,
            RSTR2   = 0x04,
            ENR1    = 0x08,
            ENR2    = 0x0C,
            ESR1    = 0x10,
            ESR2    = 0x14,
            ECR1    = 0x18,
            ECR2    = 0x1C,
            CSR     = 0x20,
            CFGR    = 0x24,
            USBCR   = 0x28,
            DLL1CR  = 0x2C,
            DLL2CR  = 0x30,
            HRCCAL1 = 0x34,
            HRCCAL2 = 0x38,
            DBGCLKR = 0x3C,
            DBGR    = 0x40,
            DWCFGR  = 0x44,
        }
    }
}
