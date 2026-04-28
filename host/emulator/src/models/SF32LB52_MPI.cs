// SF32LB52 MPI (Multi-Purpose Interface) — QSPI NOR Flash Controller
//
// Emulates the MPI peripheral with a backing NOR flash store.
// Implements the subset of SPI NOR commands used by the firmware:
//   0x06 Write Enable
//   0x05 Read Status Register 1 / 0x35 Read Status Register 2
//   0x03 Read Data
//   0x0B Fast Read
//   0x9F Read JEDEC ID
//   0x5A Read SFDP
//   0x02 Page Program (256-byte pages)
//   0x20 Sector Erase 4K
//   0x52 Block Erase 32K
//   0xD8 Block Erase 64K
//   0xC7 Chip Erase
//   0xAB Release Deep Power-Down
//   0x66/0x99 Reset Enable / Reset
//   0xB7/0xE9 Enter/Exit 4-Byte Address Mode

using System;
using System.Collections.Generic;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure.Registers;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;

namespace Antmicro.Renode.Peripherals.SPI
{
    [AllowedTranslations(AllowedTranslation.ByteToDoubleWord | AllowedTranslation.WordToDoubleWord)]
    public class SF32LB52_MPI : IDoubleWordPeripheral, IKnownSize
    {
        public SF32LB52_MPI(IMachine machine, int flashSizeBytes = 4 * 1024 * 1024)
        {
            this.machine = machine;
            this.flashSize = flashSizeBytes;
            flash = new byte[flashSizeBytes];
            // NOR flash powers up as all 0xFF
            for(int i = 0; i < flash.Length; i++)
                flash[i] = 0xFF;

            rxFifo = new Queue<uint>();
            registers = BuildRegisters();
        }

        public void Reset()
        {
            registers.Reset();
            enabled = false;
            writeEnabled = false;
            rxFifo.Clear();
            ccr1Value = 0;
            ar1Value = 0;
            dlr1Value = 0;
            txWordAccum = 0;
            txBytesInWord = 0;
            txOffset = 0;
            txTotal = 0;
            txAddress = 0;
            inWriteTransfer = false;
        }

        public uint ReadDoubleWord(long offset)
        {
            return registers.Read(offset);
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            registers.Write(offset, value);
        }

        public long Size => 0x100;

        // -----------------------------------------------------------------
        // NOR flash commands
        // -----------------------------------------------------------------

        private const byte CMD_WRITE_ENABLE   = 0x06;
        private const byte CMD_READ_STATUS_1  = 0x05;
        private const byte CMD_READ_DATA      = 0x03;
        private const byte CMD_FAST_READ      = 0x0B;
        private const byte CMD_READ_JEDEC_ID  = 0x9F;
        private const byte CMD_PAGE_PROGRAM   = 0x02;
        private const byte CMD_SECTOR_ERASE_4K = 0x20;
        private const byte CMD_BLOCK_ERASE_32K = 0x52;
        private const byte CMD_BLOCK_ERASE_64K = 0xD8;
        private const byte CMD_CHIP_ERASE     = 0xC7;
        private const byte CMD_SFDP           = 0x5A;
        private const byte CMD_READ_STATUS_2  = 0x35;
        private const byte CMD_RELEASE_DPD    = 0xAB;
        private const byte CMD_RESET_ENABLE   = 0x66;
        private const byte CMD_RESET          = 0x99;
        private const byte CMD_ENTER_4BYTE    = 0xB7;
        private const byte CMD_EXIT_4BYTE     = 0xE9;

        // GD25Q256EWIGR JEDEC ID
        private const byte JEDEC_MANUFACTURER = 0xC8; // GigaDevice
        private const byte JEDEC_MEMORY_TYPE  = 0x65;
        private const byte JEDEC_CAPACITY     = 0x19; // 32MB

        // SFDP table for a GD25Q256E-like chip (32 MB, 3/4-byte addressing,
        // single-line reads only).  Layout:
        //   0x00  8-byte SFDP global header
        //   0x08  8-byte parameter header (BFPT)
        //   0x10  64-byte BFPT body (16 DWORDs)
        private static readonly byte[] SfdpTable = BuildSfdpTable();

        private static byte[] BuildSfdpTable()
        {
            var t = new byte[0x10 + 64]; // header + PH + BFPT body

            // --- SFDP global header (8 bytes at 0x00) ---
            t[0] = 0x53; // 'S'
            t[1] = 0x46; // 'F'
            t[2] = 0x44; // 'D'
            t[3] = 0x50; // 'P'
            t[4] = 0x06; // minor rev (JESD216B)
            t[5] = 0x01; // major rev
            t[6] = 0x00; // NPH-1 = 0 → 1 parameter header
            t[7] = 0xFF; // access protocol (legacy SPI)

            // --- Parameter header 0: BFPT (8 bytes at 0x08) ---
            t[0x08] = 0x00; // ID LSB (BFPT = 0xFF00, LSB = 0x00)
            t[0x09] = 0x06; // minor version
            t[0x0A] = 0x01; // major version
            t[0x0B] = 0x10; // length = 16 DWORDs
            t[0x0C] = 0x10; // pointer low byte (BFPT body at 0x000010)
            t[0x0D] = 0x00; // pointer mid
            t[0x0E] = 0x00; // pointer high
            t[0x0F] = 0xFF; // ID MSB (BFPT = 0xFF00)

            // --- BFPT body (16 DWORDs = 64 bytes at 0x10) ---
            int bp = 0x10;

            // DWORD 1: erase/address/fast-read support bits
            //   bits 1:0  = 01  (4KB erase supported, opcode in bits 15:8)
            //   bit 2     = 1   (write granularity ≥ 64 bytes)
            //   bits 15:8 = 0x20 (4KB erase opcode)
            //   bits 18:17 = 01 (supports 3-byte and 4-byte addressing)
            //   bits 22:19 = 0  (no multi-lane fast-read in emulation)
            uint dw1 = 0x00022005;
            WriteDword(t, bp + 0, dw1);

            // DWORD 2: density — 32 MB = 256 Mbit = 0x0FFF_FFFF (bits, minus 1)
            uint dw2 = (uint)(32 * 1024 * 1024 * 8 - 1); // 0x0FFFFFFF
            WriteDword(t, bp + 4, dw2);

            // DWORDs 3-16: zero is fine — driver treats missing fast-read
            // triples as "not supported" and falls back to single-line.
            // DWORD 15 (index 14) QER field: bits 22:20 = 0 → QER::None.

            return t;
        }

        private static void WriteDword(byte[] buf, int offset, uint value)
        {
            buf[offset + 0] = (byte)(value);
            buf[offset + 1] = (byte)(value >> 8);
            buf[offset + 2] = (byte)(value >> 16);
            buf[offset + 3] = (byte)(value >> 24);
        }

        // -----------------------------------------------------------------
        // CCR1 field extraction
        // -----------------------------------------------------------------

        private int Ccr1Imode  => (int)(ccr1Value & 0x07);
        private int Ccr1Admode => (int)((ccr1Value >> 3) & 0x07);
        private int Ccr1Dmode  => (int)((ccr1Value >> 18) & 0x07);
        private bool Ccr1Fmode => ((ccr1Value >> 21) & 0x01) != 0;

        // -----------------------------------------------------------------
        // Command execution (triggered by CMDR1 write)
        // -----------------------------------------------------------------

        private void ExecuteCommand(byte cmd)
        {
            bool hasData = Ccr1Dmode != 0;
            bool isWrite = Ccr1Fmode;
            int dataLen = hasData ? (int)(dlr1Value + 1) : 0;

            switch(cmd)
            {
                case CMD_WRITE_ENABLE:
                    writeEnabled = true;
                    SetTransferComplete();
                    break;

                case CMD_READ_STATUS_1:
                    // WIP is always 0 in emulation (ops are instant)
                    rxFifo.Enqueue(0x00);
                    SetTransferComplete();
                    break;

                case CMD_READ_JEDEC_ID:
                    {
                        uint id = (uint)JEDEC_MANUFACTURER
                                | ((uint)JEDEC_MEMORY_TYPE << 8)
                                | ((uint)JEDEC_CAPACITY << 16);
                        rxFifo.Enqueue(id);
                        SetTransferComplete();
                    }
                    break;

                case CMD_READ_DATA:
                case CMD_FAST_READ:
                    {
                        uint addr = ar1Value;
                        // Push data into RX FIFO as 32-bit words
                        for(int i = 0; i < dataLen; i += 4)
                        {
                            uint word = 0;
                            for(int b = 0; b < 4 && (i + b) < dataLen; b++)
                            {
                                uint byteAddr = addr + (uint)(i + b);
                                byte val = (byteAddr < flash.Length) ? flash[byteAddr] : (byte)0xFF;
                                word |= (uint)val << (b * 8);
                            }
                            rxFifo.Enqueue(word);
                        }
                        SetTransferComplete();
                    }
                    break;

                case CMD_PAGE_PROGRAM:
                    if(isWrite && hasData)
                    {
                        // Start a write transfer — data arrives via DR writes
                        inWriteTransfer = true;
                        txAddress = ar1Value;
                        txOffset = 0;
                        txTotal = dataLen;
                        txWordAccum = 0;
                        txBytesInWord = 0;
                        // TCF is set when all data has been written via DR
                    }
                    else
                    {
                        SetTransferComplete();
                    }
                    break;

                case CMD_SECTOR_ERASE_4K:
                    EraseRegion(ar1Value, 4096);
                    SetTransferComplete();
                    break;

                case CMD_BLOCK_ERASE_32K:
                    EraseRegion(ar1Value, 32 * 1024);
                    SetTransferComplete();
                    break;

                case CMD_BLOCK_ERASE_64K:
                    EraseRegion(ar1Value, 64 * 1024);
                    SetTransferComplete();
                    break;

                case CMD_CHIP_ERASE:
                    for(int i = 0; i < flash.Length; i++)
                        flash[i] = 0xFF;
                    writeEnabled = false;
                    SetTransferComplete();
                    break;

                case CMD_SFDP:
                    {
                        uint addr = ar1Value;
                        for(int i = 0; i < dataLen; i += 4)
                        {
                            uint word = 0;
                            for(int b = 0; b < 4 && (i + b) < dataLen; b++)
                            {
                                uint byteAddr = addr + (uint)(i + b);
                                byte val = (byteAddr < SfdpTable.Length) ? SfdpTable[byteAddr] : (byte)0xFF;
                                word |= (uint)val << (b * 8);
                            }
                            rxFifo.Enqueue(word);
                        }
                        SetTransferComplete();
                    }
                    break;

                case CMD_READ_STATUS_2:
                    rxFifo.Enqueue(0x00);
                    SetTransferComplete();
                    break;

                case CMD_RELEASE_DPD:
                case CMD_RESET_ENABLE:
                case CMD_RESET:
                case CMD_ENTER_4BYTE:
                case CMD_EXIT_4BYTE:
                    SetTransferComplete();
                    break;

                default:
                    this.Log(LogLevel.Warning, "MPI: unhandled command 0x{0:X2}", cmd);
                    SetTransferComplete();
                    break;
            }
        }

        private void EraseRegion(uint address, int length)
        {
            if(!writeEnabled)
            {
                this.Log(LogLevel.Warning, "MPI: erase without write enable at 0x{0:X}", address);
                return;
            }
            for(int i = 0; i < length; i++)
            {
                uint addr = address + (uint)i;
                if(addr < flash.Length)
                    flash[addr] = 0xFF;
            }
            writeEnabled = false;
        }

        private void HandleDrWrite(uint value)
        {
            if(!inWriteTransfer)
                return;

            // Unpack up to 4 bytes from the 32-bit word
            for(int b = 0; b < 4 && txOffset < txTotal; b++, txOffset++)
            {
                byte data = (byte)(value >> (b * 8));
                uint addr = txAddress + (uint)txOffset;
                if(addr < flash.Length)
                {
                    // NOR flash: can only clear bits (AND with existing)
                    flash[addr] &= data;
                }
            }

            if(txOffset >= txTotal)
            {
                inWriteTransfer = false;
                writeEnabled = false;
                SetTransferComplete();
            }
        }

        private void SetTransferComplete()
        {
            tcf = true;
        }

        // -----------------------------------------------------------------
        // Register map
        // -----------------------------------------------------------------

        private DoubleWordRegisterCollection BuildRegisters()
        {
            var map = new Dictionary<long, DoubleWordRegister>();

            // 0x00 CR — Control Register
            map[0x00] = new DoubleWordRegister(this)
                .WithFlag(0, name: "EN",
                    writeCallback: (_, val) => enabled = val,
                    valueProviderCallback: _ => enabled);

            // 0x04 DR — Data Register (FIFO read/write)
            map[0x04] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "DATA",
                    writeCallback: (_, val) => HandleDrWrite((uint)val),
                    valueProviderCallback: _ =>
                    {
                        if(rxFifo.Count > 0)
                            return rxFifo.Dequeue();
                        return 0;
                    });

            // 0x08 DCR — Device Control Register (stub)
            map[0x08] = new DoubleWordRegister(this);

            // 0x0C PSCLR — Prescaler Register (stub, no clock model)
            map[0x0C] = new DoubleWordRegister(this)
                .WithValueField(0, 8, name: "DIV");

            // 0x10 SR — Status Register
            map[0x10] = new DoubleWordRegister(this)
                .WithFlag(0, FieldMode.Read, name: "TCF",
                    valueProviderCallback: _ => tcf)
                .WithFlag(3, FieldMode.Read, name: "SMF",
                    valueProviderCallback: _ => false)
                .WithFlag(4, FieldMode.Read, name: "CSVF",
                    valueProviderCallback: _ => false)
                .WithFlag(5, FieldMode.Read, name: "RBXF",
                    valueProviderCallback: _ => false)
                .WithFlag(31, FieldMode.Read, name: "BUSY",
                    valueProviderCallback: _ => false);

            // 0x14 SCR — Status Clear Register
            map[0x14] = new DoubleWordRegister(this)
                .WithFlag(0, FieldMode.Write, name: "TCFC",
                    writeCallback: (_, val) => { if(val) tcf = false; })
                .WithFlag(3, FieldMode.Write, name: "SMFC")
                .WithFlag(4, FieldMode.Write, name: "CSVFC")
                .WithFlag(5, FieldMode.Write, name: "RBXFC");

            // 0x18 CMDR1 — Command Register (write triggers transfer)
            map[0x18] = new DoubleWordRegister(this)
                .WithValueField(0, 8, name: "CMD",
                    writeCallback: (_, val) => ExecuteCommand((byte)val),
                    valueProviderCallback: _ => 0);

            // 0x1C AR1 — Address Register
            map[0x1C] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "ADDR",
                    writeCallback: (_, val) => ar1Value = (uint)val,
                    valueProviderCallback: _ => ar1Value);

            // 0x20 ABR1 — Alternate Byte Register (stub)
            map[0x20] = new DoubleWordRegister(this);

            // 0x24 DLR1 — Data Length Register (n-1 encoding)
            map[0x24] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "DLEN",
                    writeCallback: (_, val) => dlr1Value = (uint)val,
                    valueProviderCallback: _ => dlr1Value);

            // 0x28 CCR1 — Communication Configuration Register
            map[0x28] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "CCR1",
                    writeCallback: (_, val) => ccr1Value = (uint)val,
                    valueProviderCallback: _ => ccr1Value);

            // 0x54 FIFOCR — FIFO Control Register
            map[0x54] = new DoubleWordRegister(this)
                .WithFlag(0, FieldMode.Write, name: "RXCLR",
                    writeCallback: (_, val) => { if(val) rxFifo.Clear(); })
                .WithFlag(1, FieldMode.Read, name: "RXE",
                    valueProviderCallback: _ => rxFifo.Count == 0)
                .WithFlag(8, FieldMode.Write, name: "TXCLR")
                .WithFlag(9, FieldMode.Read, name: "TXF",
                    valueProviderCallback: _ => false);  // TX FIFO never full

            // 0x58 MISCR — Miscellaneous Register (stub)
            map[0x58] = new DoubleWordRegister(this);

            return new DoubleWordRegisterCollection(this, map);
        }

        // -----------------------------------------------------------------
        // State
        // -----------------------------------------------------------------

        private readonly IMachine machine;
        private readonly int flashSize;
        private readonly byte[] flash;
        private readonly Queue<uint> rxFifo;
        private readonly DoubleWordRegisterCollection registers;

        private bool enabled;
        private bool writeEnabled;
        private bool tcf;

        // Latched register values
        private uint ccr1Value;
        private uint ar1Value;
        private uint dlr1Value;

        // Write (page program) transfer state
        private bool inWriteTransfer;
        private uint txAddress;
        private int txOffset;
        private int txTotal;
        private uint txWordAccum;
        private int txBytesInWord;
    }
}
