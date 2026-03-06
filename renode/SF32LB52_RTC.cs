// SF32LB52 RTC — Renode peripheral model
//
// Register layout per SF32LB52x User Manual section 9.6:
//   0x00 TR    - Time Register (BCD, bits 31..11)
//   0x04 DR    - Date Register (BCD)
//   0x08 CR    - Control Register
//   0x0C ISR   - Initialization and Status Register
//   0x10 PSCLR - Prescaler Register
//   0x14 WUTR  - Wakeup Timer Register
//   0x18 ALRMTR - Alarm Time Register
//   0x1C ALRMDR - Alarm Date Register
//   0x20 SHIFTR - Shift Control Register
//   0x24 TSTR  - Timestamp Time Register
//   0x28 TSDR  - Timestamp Date Register
//   0x2C OR    - Option Register
//   0x30-0x54  - Backup Registers (BKP0R-BKP9R)
//   0x58 PBRCR - PBR Control Register
//   0x5C-0x68  - PBR0R-PBR3R
//   0x6C-0x74  - PAWK1R-PAWK3R
//
// The clock ticks once per second using a LimitTimer.
// Entering init mode (ISR.INIT=1) freezes the counter and allows
// writing TR/DR. Exiting init mode resumes counting.

using System;
using System.Collections.Generic;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure.Registers;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.Timers;
using Antmicro.Renode.Time;

namespace Antmicro.Renode.Peripherals.Timers
{
    [AllowedTranslations(AllowedTranslation.ByteToDoubleWord | AllowedTranslation.WordToDoubleWord)]
    public class SF32LB52_RTC : IDoubleWordPeripheral, IKnownSize
    {
        public SF32LB52_RTC(IMachine machine)
        {
            this.machine = machine;

            // 1 Hz tick: LimitTimer(machine, frequency, owner, divider, limit, ...)
            ticker = new LimitTimer(machine.ClockSource, 1, this, "rtc_tick",
                limit: 1,
                direction: Direction.Ascending,
                workMode: WorkMode.Periodic,
                eventEnabled: true,
                autoUpdate: true,
                enabled: true);
            ticker.LimitReached += OnSecondTick;

            registers = new DoubleWordRegisterCollection(this, BuildRegisterMap());
            Reset();
        }

        public uint ReadDoubleWord(long offset)
        {
            return registers.Read(offset);
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            registers.Write(offset, value);
        }

        public void Reset()
        {
            registers.Reset();

            var now = DateTime.UtcNow;
            hour = now.Hour;
            minute = now.Minute;
            second = now.Second;
            year = now.Year % 100;
            month = now.Month;
            day = now.Day;
            weekday = now.DayOfWeek == DayOfWeek.Sunday ? 7 : (int)now.DayOfWeek;
            centuryBit = now.Year >= 2100;

            inInit = false;
            calendarInited = true;

            prescaler = 0x80000100; // reset: DIVA_INT=0x80 @ [31:24], DIVB=0x100 @ [9:0]
            wut = 0x3FFFF;

            for(int i = 0; i < bkp.Length; i++)
                bkp[i] = 0;

            ticker.Reset();
            ticker.Enabled = true;
        }

        public long Size => 0x78;

        // ── 1-second tick ────────────────────────────────────────────

        private void OnSecondTick()
        {
            if(inInit || !calendarInited)
            {
                return;
            }

            second++;
            if(second >= 60)
            {
                second = 0;
                minute++;
                if(minute >= 60)
                {
                    minute = 0;
                    hour++;
                    if(hour >= 24)
                    {
                        hour = 0;
                        AdvanceDate();
                    }
                }
            }
        }

        private void AdvanceDate()
        {
            int fullYear = centuryBit ? 2100 + year : 2000 + year;
            int daysInMonth = DateTime.DaysInMonth(fullYear, month);

            day++;
            weekday++;
            if(weekday > 7) weekday = 1;

            if(day > daysInMonth)
            {
                day = 1;
                month++;
                if(month > 12)
                {
                    month = 1;
                    year++;
                    if(year > 99)
                    {
                        year = 0;
                        centuryBit = !centuryBit;
                    }
                }
            }
        }

        // ── Register map ─────────────────────────────────────────────

        private Dictionary<long, DoubleWordRegister> BuildRegisterMap()
        {
            var map = new Dictionary<long, DoubleWordRegister>();

            // 0x00 TR - Time Register
            map[0x00] = new DoubleWordRegister(this)
                .WithFlag(31, name: "PM",
                    valueProviderCallback: _ => false)
                .WithValueField(29, 2, name: "HT",
                    valueProviderCallback: _ => (uint)(hour / 10),
                    writeCallback: (_, val) => { if(inInit) hour = (int)val * 10 + hour % 10; })
                .WithValueField(25, 4, name: "HU",
                    valueProviderCallback: _ => (uint)(hour % 10),
                    writeCallback: (_, val) => { if(inInit) hour = hour / 10 * 10 + (int)val; })
                .WithValueField(22, 3, name: "MNT",
                    valueProviderCallback: _ => (uint)(minute / 10),
                    writeCallback: (_, val) => { if(inInit) minute = (int)val * 10 + minute % 10; })
                .WithValueField(18, 4, name: "MNU",
                    valueProviderCallback: _ => (uint)(minute % 10),
                    writeCallback: (_, val) => { if(inInit) minute = minute / 10 * 10 + (int)val; })
                .WithValueField(15, 3, name: "ST",
                    valueProviderCallback: _ => (uint)(second / 10),
                    writeCallback: (_, val) => { if(inInit) second = (int)val * 10 + second % 10; })
                .WithValueField(11, 4, name: "SU",
                    valueProviderCallback: _ => (uint)(second % 10),
                    writeCallback: (_, val) => { if(inInit) second = second / 10 * 10 + (int)val; })
                .WithReservedBits(10, 1)
                .WithValueField(0, 10, FieldMode.Read, name: "SS",
                    valueProviderCallback: _ => 0);

            // 0x04 DR - Date Register
            map[0x04] = new DoubleWordRegister(this)
                .WithFlag(31, FieldMode.Read, name: "ERR",
                    valueProviderCallback: _ => false)
                .WithReservedBits(25, 6)
                .WithFlag(24, name: "CB",
                    valueProviderCallback: _ => centuryBit,
                    writeCallback: (_, val) => { if(inInit) centuryBit = val; })
                .WithValueField(20, 4, name: "YT",
                    valueProviderCallback: _ => (uint)(year / 10),
                    writeCallback: (_, val) => { if(inInit) year = (int)val * 10 + year % 10; })
                .WithValueField(16, 4, name: "YU",
                    valueProviderCallback: _ => (uint)(year % 10),
                    writeCallback: (_, val) => { if(inInit) year = year / 10 * 10 + (int)val; })
                .WithValueField(13, 3, name: "WD",
                    valueProviderCallback: _ => (uint)weekday,
                    writeCallback: (_, val) => { if(inInit) weekday = (int)val; })
                .WithFlag(12, name: "MT",
                    valueProviderCallback: _ => month >= 10,
                    writeCallback: (_, val) => { if(inInit) month = (val ? 10 : 0) + month % 10; })
                .WithValueField(8, 4, name: "MU",
                    valueProviderCallback: _ => (uint)(month % 10),
                    writeCallback: (_, val) => { if(inInit) month = month / 10 * 10 + (int)val; })
                .WithReservedBits(6, 2)
                .WithValueField(4, 2, name: "DT",
                    valueProviderCallback: _ => (uint)(day / 10),
                    writeCallback: (_, val) => { if(inInit) day = (int)val * 10 + day % 10; })
                .WithValueField(0, 4, name: "DU",
                    valueProviderCallback: _ => (uint)(day % 10),
                    writeCallback: (_, val) => { if(inInit) day = day / 10 * 10 + (int)val; });

            // 0x08 CR - Control Register
            map[0x08] = new DoubleWordRegister(this)
                .WithReservedBits(22, 10)
                .WithFlag(21, name: "COE")
                .WithValueField(19, 2, name: "OSEL")
                .WithFlag(18, name: "POL")
                .WithFlag(17, name: "COSEL")
                .WithFlag(16, name: "BKP")
                .WithFlag(15, name: "SUB1H")
                .WithFlag(14, name: "ADD1H")
                .WithFlag(13, name: "TSIE")
                .WithFlag(12, name: "WUTIE")
                .WithFlag(11, name: "ALRMIE")
                .WithFlag(10, name: "TSE")
                .WithFlag(9, name: "WUTE")
                .WithFlag(8, name: "ALRME")
                .WithReservedBits(7, 1)
                .WithFlag(6, name: "FMT")
                .WithFlag(5, name: "BYPSHAD")
                .WithFlag(4, name: "REFCKON")
                .WithFlag(3, name: "TSEDGE")
                .WithReservedBits(2, 1)
                .WithFlag(1, name: "WUCKSEL")
                .WithFlag(0, name: "LPCKSEL");

            // 0x0C ISR - Initialization and Status Register
            // Reset value: WUTWF(bit2)=1, ALRMWF(bit0)=1 => 0x00000005
            map[0x0C] = new DoubleWordRegister(this, 0x00000005)
                .WithReservedBits(11, 21)
                .WithFlag(10, name: "INIT",
                    valueProviderCallback: _ => inInit,
                    writeCallback: (_, val) =>
                    {
                        inInit = val;
                        if(val)
                        {
                            calendarInited = true;
                            this.Log(LogLevel.Debug, "RTC: entering init mode");
                        }
                        else
                        {
                            this.Log(LogLevel.Debug, "RTC: exiting init mode, clock resumes");
                        }
                    })
                .WithFlag(9, FieldMode.Read, name: "INITF",
                    valueProviderCallback: _ => inInit)
                .WithFlag(8, FieldMode.Read, name: "INITS",
                    valueProviderCallback: _ => calendarInited)
                .WithFlag(7, name: "RSF")
                .WithFlag(6, FieldMode.Read, name: "SHPF",
                    valueProviderCallback: _ => false)
                .WithFlag(5, FieldMode.Read, name: "TSOVF",
                    valueProviderCallback: _ => false)
                .WithFlag(4, name: "TSF")
                .WithFlag(3, name: "WUTF")
                .WithFlag(2, FieldMode.Read, name: "WUTWF",
                    valueProviderCallback: _ => true)
                .WithFlag(1, name: "ALRMF")
                .WithFlag(0, FieldMode.Read, name: "ALRMWF",
                    valueProviderCallback: _ => true);

            // 0x10 PSCLR - Prescaler Register
            // Reset: DIVA_INT[31:24]=0x80, DIVA_FRAC[23:10]=0, DIVB[9:0]=0x100
            map[0x10] = new DoubleWordRegister(this, 0x80000100)
                .WithValueField(24, 8, name: "DIVA_INT")
                .WithValueField(10, 14, name: "DIVA_FRAC")
                .WithValueField(0, 10, name: "DIVB");

            // 0x14 WUTR - Wakeup Timer Register
            map[0x14] = new DoubleWordRegister(this, 0x0003FFFF)
                .WithReservedBits(18, 14)
                .WithValueField(0, 18, name: "WUT");

            // 0x18 ALRMTR - Alarm Time Register (stub)
            map[0x18] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "ALRMTR");

            // 0x1C ALRMDR - Alarm Date Register (stub)
            map[0x1C] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "ALRMDR");

            // 0x20 SHIFTR - Shift Control Register (stub)
            map[0x20] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "SHIFTR");

            // 0x24 TSTR - Timestamp Time Register (read-only, stub)
            map[0x24] = new DoubleWordRegister(this)
                .WithValueField(0, 32, FieldMode.Read, name: "TSTR");

            // 0x28 TSDR - Timestamp Date Register (read-only, stub)
            map[0x28] = new DoubleWordRegister(this)
                .WithValueField(0, 32, FieldMode.Read, name: "TSDR");

            // 0x2C OR - Option Register (stub)
            map[0x2C] = new DoubleWordRegister(this)
                .WithReservedBits(2, 30)
                .WithFlag(1, name: "RTC_OUT_RMP")
                .WithFlag(0, name: "RTC_ALARM_TYPE");

            // 0x30-0x54 BKP0R-BKP9R - Backup Registers
            for(int i = 0; i < 10; i++)
            {
                int idx = i;
                map[0x30 + i * 4] = new DoubleWordRegister(this)
                    .WithValueField(0, 32, name: $"BKP{i}",
                        valueProviderCallback: _ => bkp[idx],
                        writeCallback: (_, val) => bkp[idx] = (uint)val);
            }

            // 0x58 PBRCR - PBR Control Register
            map[0x58] = new DoubleWordRegister(this, 0x03) // reset: SNS=1, RTO=1
                .WithReservedBits(8, 24)
                .WithValueField(4, 4, name: "DBG_SEL")
                .WithReservedBits(2, 2)
                .WithFlag(1, name: "SNS")
                .WithFlag(0, name: "RTO");

            // 0x5C-0x68 PBR0R-PBR3R (stubs with reset DS0=1, IS=1 => 0xA0)
            for(int i = 0; i < 4; i++)
            {
                map[0x5C + i * 4] = new DoubleWordRegister(this, 0xA0)
                    .WithValueField(0, 32, name: $"PBR{i}R");
            }

            // 0x6C PAWK1R
            map[0x6C] = new DoubleWordRegister(this, 0x00000040)
                .WithValueField(0, 32, name: "PAWK1R");

            // 0x70 PAWK2R
            map[0x70] = new DoubleWordRegister(this, 0x00000000)
                .WithValueField(0, 32, name: "PAWK2R");

            // 0x74 PAWK3R
            map[0x74] = new DoubleWordRegister(this, 0x0001FFFF)
                .WithValueField(0, 32, name: "PAWK3R");

            return map;
        }

        // ── State ────────────────────────────────────────────────────

        private readonly IMachine machine;
        private readonly DoubleWordRegisterCollection registers;
        private readonly LimitTimer ticker;

        private int hour, minute, second;
        private int year, month, day, weekday;
        private bool centuryBit;
        private bool inInit;
        private bool calendarInited;
        private uint[] bkp = new uint[10];
        private uint prescaler;
        private uint wut;
    }
}
