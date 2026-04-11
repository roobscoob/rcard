//
// SiFli SF32LB52x HPSYS_AON peripheral model for Renode.
// Base address: 0x500C0000, size 0x40.
//
// Models the HPSYS always-on block. Behavioral bits:
//   * PMR.FORCE_SLEEP -- write-1-self-clearing
//   * ACR.HXT48_RDY / HRC48_RDY -- read 1 whenever the matching *_REQ bit is
//     set, so firmware that does
//         ACR |= HXT48_REQ; while(!(ACR & HXT48_RDY));
//     proceeds immediately
//   * ISSR.HP_ACTIVE / LP_ACTIVE -- start asserted (both subsystems alive)
//   * GTIMR.CNT -- monotonic 32-bit counter driven from machine virtual time
//   * WCR -- write-1-clear, accepted as no-op (we have no real wakeup IRQs)
//   * WSR -- always reads 0
// Everything else is plain storage.
//
// Reference: UM5201-SF32LB52x User Manual v0.8.4, section 4.3.
//
// Drop into: src/Infrastructure/src/Emulator/Peripherals/Peripherals/Miscellaneous/
//
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure.Registers;
using Antmicro.Renode.Peripherals.Bus;

namespace Antmicro.Renode.Peripherals.Miscellaneous
{
    public class SF32LB52_HPSYS_AON : BasicDoubleWordPeripheral, IKnownSize
    {
        public SF32LB52_HPSYS_AON(IMachine machine) : base(machine)
        {
            this.machine = machine;
            DefineRegisters();
        }

        public long Size => 0x40;

        private void DefineRegisters()
        {
            // 0x00 PMR -- power mode. FORCE_SLEEP self-clears on read.
            Registers.PMR.Define(this)
                .WithValueField(0, 2, name: "MODE")
                .WithReservedBits(2, 28)
                .WithFlag(30, name: "FORCE_LCPU")
                .WithFlag(31, FieldMode.Write | FieldMode.Read, name: "FORCE_SLEEP",
                    valueProviderCallback: _ => false);

            // 0x04 CR1 -- pin wake mode 0..3, plus debug bits. Storage only.
            Registers.CR1.Define(this);
            // 0x08 CR2 -- pin wake mode 10..15.
            Registers.CR2.Define(this);
            // 0x0C CR3 -- pin wake mode 16..20.
            Registers.CR3.Define(this);

            // 0x10 ACR -- Active mode clock requests. The *_RDY bits report
            // back the *_REQ value so polling loops finish in zero ticks.
            Registers.ACR.Define(this, 0x07)
                .WithFlag(0, out hrc48Req, name: "HRC48_REQ")
                .WithFlag(1, out hxt48Req, name: "HXT48_REQ")
                .WithFlag(2, name: "PWR_REQ")
                .WithFlag(3, name: "EXTPWR_REQ")
                .WithReservedBits(4, 26)
                .WithFlag(30, FieldMode.Read, name: "HRC48_RDY",
                    valueProviderCallback: _ => hrc48Req.Value)
                .WithFlag(31, FieldMode.Read, name: "HXT48_RDY",
                    valueProviderCallback: _ => hxt48Req.Value);

            // 0x14 LSCR / 0x18 DSCR / 0x1C SBCR -- per-mode clock requests,
            // storage only with the manual's reset values.
            Registers.LSCR.Define(this, 0x07);
            Registers.DSCR.Define(this, 0x04);
            Registers.SBCR.Define(this);

            // 0x20 WER -- wakeup enable mask, storage.
            Registers.WER.Define(this);

            // 0x24 WSR -- wakeup status, masked by WER. We have no wakeup
            // sources to model so this stays 0.
            Registers.WSR.Define(this)
                .WithValueField(0, 32, FieldMode.Read, name: "WSR",
                    valueProviderCallback: _ => 0);

            // 0x28 WCR -- write-1-clear wakeup. Accept and discard.
            Registers.WCR.Define(this)
                .WithValueField(0, 32, FieldMode.Write, name: "WCR");

            // 0x2C ISSR -- inter-subsystem state. Both halves start active.
            Registers.ISSR.Define(this, 0x30)
                .WithFlag(0, name: "HP2LP_REQ")
                .WithFlag(1, FieldMode.Read, name: "LP2HP_REQ",
                    valueProviderCallback: _ => false)
                .WithReservedBits(2, 2)
                .WithFlag(4, name: "HP_ACTIVE")
                .WithFlag(5, FieldMode.Read, name: "LP_ACTIVE",
                    valueProviderCallback: _ => true)
                .WithReservedBits(6, 26);

            // 0x30 ANACR -- analog isolation, storage.
            Registers.ANACR.Define(this);

            // 0x34 GTIMR -- 32-bit free-running global timer. Tied to virtual
            // time so firmware delay loops based on this register actually
            // advance. Wraps naturally at 2^32 microseconds (~71 minutes).
            Registers.GTIMR.Define(this)
                .WithValueField(0, 32, FieldMode.Read, name: "CNT",
                    valueProviderCallback: _ =>
                        (uint)(machine.ElapsedVirtualTime.TimeElapsed.Ticks / 10));

            // 0x38 / 0x3C reserved scratch registers, storage.
            Registers.RESERVE0.Define(this);
            Registers.RESERVE1.Define(this);
        }

        private new readonly IMachine machine;
        private IFlagRegisterField hrc48Req;
        private IFlagRegisterField hxt48Req;

        private enum Registers : long
        {
            PMR      = 0x00,
            CR1      = 0x04,
            CR2      = 0x08,
            CR3      = 0x0C,
            ACR      = 0x10,
            LSCR     = 0x14,
            DSCR     = 0x18,
            SBCR     = 0x1C,
            WER      = 0x20,
            WSR      = 0x24,
            WCR      = 0x28,
            ISSR     = 0x2C,
            ANACR    = 0x30,
            GTIMR    = 0x34,
            RESERVE0 = 0x38,
            RESERVE1 = 0x3C,
        }
    }
}
