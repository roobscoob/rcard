// SF32LB52 SDMMC — Renode peripheral model
//
// Register layout per SF32LB52x User Manual section 14.1:
//   0x00  SR     - Status Register
//   0x04  CCR    - Command Control Register
//   0x08  CAR    - Command Argument Register
//   0x0C  RIR    - Response Command Index Register
//   0x10  RAR1   - Response Argument 1
//   0x14  RAR2   - Response Argument 2
//   0x18  RAR3   - Response Argument 3
//   0x1C  RAR4   - Response Argument 4
//   0x20  TOR    - Timeout Register
//   0x24  DCR    - Data Control Register
//   0x28  DLR    - Data Length Register
//   0x2C  IER    - Interrupt Enable Register
//   0x30  CLKCR  - Clock Control Register
//   0x3C  CDR    - Card Detect Register
//   0x40  DBGR1  - Debug Register 1
//   0x44  DBGR2  - Debug Register 2
//   0x48  CEATA  - CE-ATA/SDIO Register
//   0x54  DSR    - Data Status Register
//   0x58  CDCR   - Clock Duty Cycle Register
//   0x5C  CASR   - Cache Status Register
//   0x60  CACR   - Cache Control Register
//   0x64  CACNT  - Cache Counter Register
//   0x68  CAOFF  - Cache Offset Register
//   0x200-0x3FC  FIFO
//
// Implements enough SD protocol to support basic card init, read, and write.

using System;
using System.Collections.Generic;
using System.IO;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure.Registers;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;

namespace Antmicro.Renode.Peripherals.SD
{
    [AllowedTranslations(AllowedTranslation.ByteToDoubleWord | AllowedTranslation.WordToDoubleWord)]
    public class SF32LB52_SDMMC : IDoubleWordPeripheral, IKnownSize, IGPIOSender, IDisposable
    {
        public SF32LB52_SDMMC(IMachine machine, long cardSizeBytes = 128 * 1024 * 1024,
                               string imagePath = null)
        {
            this.machine = machine;
            this.cardSize = cardSizeBytes;
            this.imagePath = imagePath ?? Environment.GetEnvironmentVariable("RCARD_SDMMC_IMG");
            fifo = new Queue<uint>();
            IRQ = new GPIO();

            if(this.imagePath != null)
            {
                // Open or create the backing file
                backingFile = new FileStream(this.imagePath, FileMode.OpenOrCreate, FileAccess.ReadWrite);
                if(backingFile.Length < cardSizeBytes)
                {
                    // Extend the file to the full card size (sparse on NTFS)
                    backingFile.SetLength(cardSizeBytes);
                }
                storage = new byte[cardSizeBytes];
                backingFile.Position = 0;
                int toRead = (int)Math.Min(cardSizeBytes, backingFile.Length);
                int offset = 0;
                while(offset < toRead)
                {
                    int n = backingFile.Read(storage, offset, toRead - offset);
                    if(n == 0) break;
                    offset += n;
                }
            }
            else
            {
                storage = new byte[cardSizeBytes];
            }

            registers = new DoubleWordRegisterCollection(this, BuildRegisterMap());
            Reset();
        }

        public uint ReadDoubleWord(long offset)
        {
            // FIFO region: 0x200-0x3FC
            if(offset >= 0x200 && offset <= 0x3FC)
            {
                return ReadFifo();
            }
            return registers.Read(offset);
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            // FIFO region: 0x200-0x3FC
            if(offset >= 0x200 && offset <= 0x3FC)
            {
                WriteFifo(value);
                return;
            }
            registers.Write(offset, value);
        }

        /// Load a disk image into the peripheral's backing storage.
        /// Called via the Renode monitor: sdmmc1 LoadImage @path/to/file
        public void LoadImage(string path)
        {
            using(var fs = new FileStream(path, FileMode.Open, FileAccess.Read))
            {
                int toRead = (int)Math.Min(cardSize, fs.Length);
                int offset = 0;
                while(offset < toRead)
                {
                    int n = fs.Read(storage, offset, toRead - offset);
                    if(n == 0) break;
                    offset += n;
                }
            }
            this.Log(LogLevel.Info, "SDMMC: Loaded {0} byte image from {1}", cardSize, path);
        }

        public void Reset()
        {
            registers.Reset();
            fifo.Clear();

            rspIndex = 0;
            rspArg1 = 0;
            rspArg2 = 0;
            rspArg3 = 0;
            rspArg4 = 0;

            srValue = 0;
            ierValue = 0x000206EE; // reset value: most interrupt sources masked
            cmdIndex = 0;
            cmdArg = 0;
            cmdHasRsp = true;
            cmdLongRsp = false;
            ccrTxEn = false;
            ccrPend = false;

            dataReadMode = false;
            dataWriteMode = false;
            dataOffset = 0;
            dataRemaining = 0;
            dataLen = 0x1FF; // reset: 512 bytes
            blockSize = 512;
            blocksTransferred = 0;
            writeWordCount = 0;

            nextIsAppCmd = false;
            cardSelected = false;
            cardRCA = 0x0001;

            IRQ.Unset();
        }

        public long Size => 0x400;

        public GPIO IRQ { get; }

        // ── SD Command Processing ──────────────────────────────────────

        private void ExecuteCommand()
        {
            rspIndex = 0;
            rspArg1 = 0;
            rspArg2 = 0;
            rspArg3 = 0;
            rspArg4 = 0;

            // Clear status bits that get auto-cleared on new command
            srValue &= ~(SR_CMD_DONE | SR_CMD_TIMEOUT | SR_CMD_RSP_CRC | SR_CMD_SENT);

            if(nextIsAppCmd)
            {
                nextIsAppCmd = false;
                ProcessAppCommand();
            }
            else
            {
                ProcessCommand();
            }

            // Set cmd_done and cmd_sent
            srValue |= SR_CMD_DONE | SR_CMD_SENT;

            this.Log(LogLevel.Debug, "SDMMC: CMD{0} arg=0x{1:X08} -> rsp_index={2} rsp_arg1=0x{3:X08}",
                cmdIndex, cmdArg, rspIndex, rspArg1);

            UpdateInterrupt();
        }

        private void ProcessCommand()
        {
            switch(cmdIndex)
            {
                case 0: // GO_IDLE_STATE
                    cardSelected = false;
                    break;

                case 2: // ALL_SEND_CID - R2 (136-bit)
                    rspIndex = 0x3F;
                    FillCID();
                    break;

                case 3: // SEND_RELATIVE_ADDR - R6
                    rspIndex = 3;
                    rspArg1 = (uint)(cardRCA << 16) | 0x0500; // ready state
                    break;

                case 7: // SELECT/DESELECT_CARD - R1b
                    rspIndex = 7;
                    cardSelected = (cmdArg >> 16) == cardRCA;
                    rspArg1 = cardSelected ? 0x00000700u : 0u; // transfer state
                    break;

                case 8: // SEND_IF_COND - R7
                    rspIndex = 8;
                    rspArg1 = cmdArg & 0xFFF; // echo back voltage + check pattern
                    break;

                case 9: // SEND_CSD - R2 (136-bit)
                    rspIndex = 0x3F;
                    BuildCSD();
                    break;

                case 10: // SEND_CID - R2
                    rspIndex = 0x3F;
                    FillCID();
                    break;

                case 12: // STOP_TRANSMISSION - R1b
                    rspIndex = 12;
                    rspArg1 = 0x00000900; // transfer state
                    dataReadMode = false;
                    dataWriteMode = false;
                    dataRemaining = 0;
                    srValue |= SR_DATA_DONE;
                    srValue &= ~SR_DATA_BUSY;
                    break;

                case 13: // SEND_STATUS - R1
                    rspIndex = 13;
                    rspArg1 = cardSelected ? 0x00000900u : 0x00000500u;
                    break;

                case 16: // SET_BLOCKLEN - R1
                    rspIndex = 16;
                    rspArg1 = 0x00000900;
                    break;

                case 17: // READ_SINGLE_BLOCK - R1
                    rspIndex = 17;
                    rspArg1 = 0x00000900;
                    PrepareRead(cmdArg, 1);
                    break;

                case 18: // READ_MULTIPLE_BLOCK - R1
                    rspIndex = 18;
                    rspArg1 = 0x00000900;
                    // Number of blocks determined by data_len register
                    PrepareRead(cmdArg, -1);
                    break;

                case 24: // WRITE_BLOCK - R1
                    rspIndex = 24;
                    rspArg1 = 0x00000D00; // receive data state
                    PrepareWrite(cmdArg, 1);
                    break;

                case 25: // WRITE_MULTIPLE_BLOCK - R1
                    rspIndex = 25;
                    rspArg1 = 0x00000D00;
                    PrepareWrite(cmdArg, -1);
                    break;

                case 55: // APP_CMD - R1
                    rspIndex = 55;
                    rspArg1 = 0x00000120; // APP_CMD bit set
                    nextIsAppCmd = true;
                    break;

                default:
                    this.Log(LogLevel.Warning, "SDMMC: Unhandled CMD{0} arg=0x{1:X08}", cmdIndex, cmdArg);
                    rspIndex = (uint)cmdIndex;
                    rspArg1 = 0x00000900;
                    break;
            }
        }

        private void ProcessAppCommand()
        {
            switch(cmdIndex)
            {
                case 6: // SET_BUS_WIDTH - R1
                    rspIndex = 6;
                    rspArg1 = 0x00000920;
                    break;

                case 41: // SD_SEND_OP_COND - R3
                    rspIndex = 0x3F;
                    // Card ready, SDHC supported, 3.3V
                    rspArg1 = 0xC0FF8000;
                    break;

                case 51: // SEND_SCR - R1 + data
                    rspIndex = 51;
                    rspArg1 = 0x00000920;
                    // Queue SCR data (8 bytes = 2 words) into FIFO
                    fifo.Clear();
                    fifo.Enqueue(0x02358000); // SD spec 2.0, 4-bit bus supported
                    fifo.Enqueue(0x00000000);
                    srValue |= SR_DATA_DONE;
                    break;

                default:
                    this.Log(LogLevel.Warning, "SDMMC: Unhandled ACMD{0} arg=0x{1:X08}", cmdIndex, cmdArg);
                    rspIndex = (uint)cmdIndex;
                    rspArg1 = 0x00000920;
                    break;
            }
        }

        // ── CID (MK SD NAND) ────────────────────────────────────────────
        //
        // 128-bit CID mapped into RAR4..RAR1 the same way as CSD:
        //   RAR4[23:0]  = CID[127:104]  (MID, OID)
        //   RAR3[31:0]  = CID[103:72]   (PNM[39:8])
        //   RAR2[31:0]  = CID[71:40]    (PNM[7:0], PRV, PSN[31:16])
        //   RAR1[31:0]  = CID[39:8]     (PSN[15:0], reserved, MDT, CRC)

        private void FillCID()
        {
            rspArg4 = 0x222345;    // MID=0x22, OID="#E"
            rspArg3 = 0x4D4B0000;  // PNM="MK\0\0\0" (upper 4 bytes)
            rspArg2 = 0x0006150C;  // PNM low byte=0x00, PRV=0x06, PSN[31:16]=0x150C
            rspArg1 = 0x0415021C;  // PSN[15:0]=0x0415, MDT=0x21C (2018, Dec)
        }

        // ── CSD for an SDHC card (CSD Version 2.0, SD Phys Layer 4.10 sec 5.3.3) ──
        //
        // The 128-bit CSD is returned as a 136-bit R2 response. The controller
        // strips the start bit, transmission bit, and 6 reserved bits, then
        // stores bits [127:8] across RAR4..RAR1:
        //   RAR4[23:0]  = CSD[127:104]
        //   RAR3[31:0]  = CSD[103:72]
        //   RAR2[31:0]  = CSD[71:40]
        //   RAR1[31:0]  = CSD[39:8]
        //
        // CSD v2 layout (bit positions within the 128-bit register):
        //   [127:126] CSD_STRUCTURE = 01
        //   [125:120] reserved = 0
        //   [119:112] TAAC = 0x0E (1ms)
        //   [111:104] NSAC = 0x00
        //   [103:96]  TRAN_SPEED = 0x32 (25MHz)
        //   [95:84]   CCC = 0x5B5 (classes 0,2,4,5,7,8,10)
        //   [83:80]   READ_BL_LEN = 9 (512 bytes)
        //   [79:70]   flags (READ_BL_PARTIAL=0, WRITE_BLK_MISALIGN=0, etc)
        //   [69:48]   C_SIZE = (cardSize / (512*1024)) - 1
        //   [47]      reserved = 0
        //   [46]      ERASE_BLK_EN = 1
        //   [45:39]   SECTOR_SIZE = 0x7F (128 blocks)
        //   [38:32]   WP_GRP_SIZE=0, WP_GRP_ENABLE=0, R2W_FACTOR=010, WRITE_BL_LEN=9
        //   [31:8]    flags, FILE_FORMAT=0, CRC

        private void BuildCSD()
        {
            uint cSize = (uint)(cardSize / (512 * 1024)) - 1; // 128MB => C_SIZE = 255

            // CSD[127:104] -> RAR4[23:0]
            // [127:126]=01 (CSD v2), [125:120]=0, [119:112]=0x0E (TAAC), [111:104]=0x00 (NSAC)
            rspArg4 = 0x400E00;

            // CSD[103:72] -> RAR3[31:0]
            // [103:96]=0x32 (TRAN_SPEED 25MHz), [95:84]=0x5B5 (CCC), [83:80]=0x9 (READ_BL_LEN)
            // [79:72]=0x00 (partial/misalign flags all 0)
            rspArg3 = 0x325B5900;

            // CSD[71:40] -> RAR2[31:0]
            // [71:70]=0 (DSR_IMP=0, reserved), [69:48]=cSize (22 bits)
            // [47]=0 (reserved), [46]=1 (ERASE_BLK_EN), [45:39]=0x7F (SECTOR_SIZE)
            rspArg2 = ((cSize & 0x3FFFFF) << 8) | 0x7F;

            // CSD[39:8] -> RAR1[31:0]
            // [39]=SECTOR_SIZE LSB=1, [38:32]=WP_GRP_SIZE=0x00
            // [31]=WP_GRP_ENABLE=0, [30:29]=reserved, [28:26]=R2W_FACTOR=010
            // [25:22]=WRITE_BL_LEN=9 (0b1001)
            // [21]=WRITE_BL_PARTIAL=0, [20:16]=reserved
            // [15]=FILE_FORMAT_GRP=0, [14]=COPY=0, [13]=PERM_WRITE_PROTECT=0
            // [12]=TMP_WRITE_PROTECT=0, [11:10]=FILE_FORMAT=00, [9:8]=reserved
            rspArg1 = 0x800A4000;
        }

        // ── Data Transfer ──────────────────────────────────────────────

        private void PrepareRead(uint address, int blockCount)
        {
            // For SDHC cards, address is in blocks (512-byte units)
            dataOffset = (long)address * 512;
            dataReadMode = true;
            dataWriteMode = false;
            blocksTransferred = 0;

            if(blockCount > 0)
            {
                dataRemaining = blockCount * blockSize;
            }
            else
            {
                // Multi-block: use data_len register
                dataRemaining = (int)(dataLen + 1);
            }

            // Pre-fill FIFO with first chunk
            FillReadFifo();
        }

        private void PrepareWrite(uint address, int blockCount)
        {
            dataOffset = (long)address * 512;
            dataWriteMode = true;
            dataReadMode = false;
            blocksTransferred = 0;

            if(blockCount > 0)
            {
                dataRemaining = blockCount * blockSize;
            }
            else
            {
                dataRemaining = (int)(dataLen + 1);
            }

            srValue |= SR_DATA_BUSY;
        }

        private void FillReadFifo()
        {
            fifo.Clear();

            // Fill up to one block worth of data
            int toRead = Math.Min(dataRemaining, blockSize);
            for(int i = 0; i < toRead; i += 4)
            {
                uint word = 0;
                for(int b = 0; b < 4 && (i + b) < toRead; b++)
                {
                    long addr = dataOffset + i + b;
                    byte val = (addr >= 0 && addr < cardSize) ? storage[addr] : (byte)0xFF;
                    word |= (uint)val << (b * 8);
                }
                fifo.Enqueue(word);
            }

            dataOffset += toRead;
            dataRemaining -= toRead;
            blocksTransferred++;

            if(dataRemaining <= 0)
            {
                srValue |= SR_DATA_DONE;
                srValue &= ~SR_DATA_BUSY;
                dataReadMode = false;
            }
            else
            {
                srValue |= SR_DATA_BUSY;
            }
        }

        private uint ReadFifo()
        {
            if(fifo.Count > 0)
            {
                uint val = fifo.Dequeue();

                // If FIFO emptied and more data to read, refill
                if(fifo.Count == 0 && dataReadMode && dataRemaining > 0)
                {
                    FillReadFifo();
                    UpdateInterrupt();
                }

                return val;
            }

            this.Log(LogLevel.Warning, "SDMMC: FIFO underrun on read");
            srValue |= SR_FIFO_UNDERRUN;
            return 0;
        }

        private void WriteFifo(uint value)
        {
            if(!dataWriteMode || dataRemaining <= 0)
            {
                this.Log(LogLevel.Warning, "SDMMC: FIFO write when not in write mode");
                return;
            }

            // Write 4 bytes to storage
            for(int b = 0; b < 4 && dataRemaining > 0; b++)
            {
                if(dataOffset >= 0 && dataOffset < cardSize)
                {
                    storage[dataOffset] = (byte)(value >> (b * 8));
                }
                dataOffset++;
                dataRemaining--;
            }

            writeWordCount++;
            if(writeWordCount >= blockSize / 4)
            {
                writeWordCount = 0;
                blocksTransferred++;
            }

            if(dataRemaining <= 0)
            {
                srValue |= SR_DATA_DONE;
                srValue &= ~SR_DATA_BUSY;
                dataWriteMode = false;
                FlushToDisk();
                UpdateInterrupt();
            }
        }

        // ── Disk persistence ─────────────────────────────────────────────

        private void FlushToDisk()
        {
            if(backingFile == null)
            {
                return;
            }
            backingFile.Position = 0;
            backingFile.Write(storage, 0, (int)cardSize);
            backingFile.Flush();
        }

        public void Dispose()
        {
            if(backingFile != null)
            {
                FlushToDisk();
                backingFile.Dispose();
                backingFile = null;
            }
        }

        // ── Interrupt ──────────────────────────────────────────────────

        private void UpdateInterrupt()
        {
            // IER masks: bit set = masked (no interrupt)
            // Interrupt fires when status bit is set AND mask bit is clear
            uint active = srValue & ~ierValue & SR_IRQ_MASK;
            if(active != 0)
            {
                IRQ.Set(true);
            }
            else
            {
                IRQ.Set(false);
            }
        }

        // ── Register Map ───────────────────────────────────────────────

        private Dictionary<long, DoubleWordRegister> BuildRegisterMap()
        {
            var map = new Dictionary<long, DoubleWordRegister>();

            // 0x00 SR - Status Register
            map[0x00] = new DoubleWordRegister(this)
                .WithValueField(0, 18, name: "SR",
                    valueProviderCallback: _ =>
                    {
                        // card_exist is always set (card is inserted)
                        return srValue | SR_CARD_EXIST;
                    },
                    writeCallback: (_, val) =>
                    {
                        // Writing 1 clears the W1C bits
                        uint clearMask = (uint)val & SR_W1C_MASK;
                        srValue &= ~clearMask;
                        UpdateInterrupt();
                    });

            // 0x04 CCR - Command Control Register
            // Handled as a single field to avoid field-ordering issues: Renode
            // processes fields LSB-first, so a per-bit layout would fire cmd_start
            // before cmd_index is updated. Instead we parse the whole word on write.
            map[0x04] = new DoubleWordRegister(this, 0x00010000) // reset: cmd_has_rsp=1
                .WithValueField(0, 24, name: "CCR",
                    valueProviderCallback: _ =>
                    {
                        uint v = 0;
                        // cmd_start always reads 0 (auto-clears)
                        if(ccrTxEn)    v |= 1u << 8;
                        if(ccrPend)    v |= 1u << 9;
                        if(cmdHasRsp)  v |= 1u << 16;
                        if(cmdLongRsp) v |= 1u << 17;
                        v |= (cmdIndex & 0x3F) << 18;
                        return v;
                    },
                    writeCallback: (_, val) =>
                    {
                        uint v = (uint)val;
                        ccrTxEn    = (v & (1u << 8)) != 0;
                        ccrPend    = (v & (1u << 9)) != 0;
                        cmdHasRsp  = (v & (1u << 16)) != 0;
                        cmdLongRsp = (v & (1u << 17)) != 0;
                        cmdIndex   = (v >> 18) & 0x3F;

                        // cmd_start triggers command execution after all fields are latched
                        if((v & 1u) != 0)
                        {
                            ExecuteCommand();
                        }
                    })
                .WithReservedBits(24, 8);

            // 0x08 CAR - Command Argument Register
            map[0x08] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "cmd_arg",
                    writeCallback: (_, val) => cmdArg = (uint)val,
                    valueProviderCallback: _ => cmdArg);

            // 0x0C RIR - Response Command Index Register
            map[0x0C] = new DoubleWordRegister(this)
                .WithValueField(0, 6, FieldMode.Read, name: "rsp_index",
                    valueProviderCallback: _ => rspIndex)
                .WithReservedBits(6, 26);

            // 0x10 RAR1
            map[0x10] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "rsp_arg1",
                    valueProviderCallback: _ => rspArg1,
                    writeCallback: (_, val) => rspArg1 = (uint)val);

            // 0x14 RAR2
            map[0x14] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "rsp_arg2",
                    valueProviderCallback: _ => rspArg2,
                    writeCallback: (_, val) => rspArg2 = (uint)val);

            // 0x18 RAR3
            map[0x18] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "rsp_arg3",
                    valueProviderCallback: _ => rspArg3,
                    writeCallback: (_, val) => rspArg3 = (uint)val);

            // 0x1C RAR4
            map[0x1C] = new DoubleWordRegister(this)
                .WithValueField(0, 24, name: "rsp_arg4",
                    valueProviderCallback: _ => rspArg4,
                    writeCallback: (_, val) => rspArg4 = (uint)val)
                .WithReservedBits(24, 8);

            // 0x20 TOR - Timeout Register
            map[0x20] = new DoubleWordRegister(this, 0x00001000)
                .WithValueField(0, 32, name: "timeout_cnt");

            // 0x24 DCR - Data Control Register
            map[0x24] = new DoubleWordRegister(this, 0x01FF0000) // block_size reset = 0x1FF (512 bytes)
                .WithFlag(0, name: "data_start",
                    valueProviderCallback: _ => false,
                    writeCallback: (_, val) =>
                    {
                        if(val)
                        {
                            srValue &= ~(SR_DATA_DONE | SR_DATA_CRC | SR_DATA_TIMEOUT);
                            if(dataReadMode)
                            {
                                srValue |= SR_DATA_BUSY;
                            }
                        }
                    })
                .WithReservedBits(1, 7)
                .WithFlag(8, name: "tran_data_en")
                .WithFlag(9, name: "r_wn",
                    writeCallback: (_, val) => { /* tracked via PrepareRead/Write */ })
                .WithFlag(10, name: "stream_mode")
                .WithValueField(11, 2, name: "wire_mode")
                .WithReservedBits(13, 3)
                .WithValueField(16, 11, name: "block_size",
                    writeCallback: (_, val) => blockSize = (int)(val + 1),
                    valueProviderCallback: _ => (uint)(blockSize - 1));

            // 0x28 DLR - Data Length Register
            map[0x28] = new DoubleWordRegister(this, 0x000001FF) // reset: data_len=0x1FF (512 bytes)
                .WithValueField(0, 16, name: "data_len",
                    writeCallback: (_, val) => dataLen = (uint)val,
                    valueProviderCallback: _ => dataLen)
                .WithValueField(16, 16, FieldMode.Read, name: "block_tran_num",
                    valueProviderCallback: _ => (uint)blocksTransferred);

            // 0x2C IER - Interrupt Enable (Mask) Register
            // Reset: many masks set to 1 (masked/disabled)
            map[0x2C] = new DoubleWordRegister(this, 0x000206EE)
                .WithValueField(0, 18, name: "IER",
                    writeCallback: (_, val) =>
                    {
                        ierValue = (uint)val;
                        UpdateInterrupt();
                    },
                    valueProviderCallback: _ => ierValue);

            // 0x30 CLKCR - Clock Control Register
            map[0x30] = new DoubleWordRegister(this, 0x00008001) // reset: div=0x80, stop_clk=1
                .WithFlag(0, name: "stop_clk")
                .WithFlag(1, name: "void_fifo_error")
                .WithValueField(2, 2, name: "clk_tune_sel")
                .WithReservedBits(4, 4)
                .WithValueField(8, 13, name: "div")
                .WithReservedBits(21, 11);

            // 0x3C CDR - Card Detect Register
            map[0x3C] = new DoubleWordRegister(this, 0x19) // reset: sd_data3_cd=1, en_cd=1, cd_hvalid=1
                .WithFlag(0, name: "sd_data3_cd")
                .WithFlag(1, name: "itiming_sel")
                .WithFlag(2, name: "otiming_sel")
                .WithFlag(3, name: "en_cd")
                .WithFlag(4, name: "cd_hvalid")
                .WithFlag(5, name: "cmd_od")
                .WithValueField(6, 13, name: "itiming")
                .WithValueField(19, 13, name: "otiming");

            // 0x40 DBGR1
            map[0x40] = new DoubleWordRegister(this, 0x00010001)
                .WithValueField(0, 16, FieldMode.Read, name: "cmd_st",
                    valueProviderCallback: _ => 1)
                .WithValueField(16, 15, FieldMode.Read, name: "data_st",
                    valueProviderCallback: _ => 1)
                .WithReservedBits(31, 1);

            // 0x44 DBGR2
            map[0x44] = new DoubleWordRegister(this)
                .WithValueField(0, 14, FieldMode.Read, name: "host_word_counter")
                .WithReservedBits(14, 2)
                .WithValueField(16, 10, FieldMode.Read, name: "valid_data_cou")
                .WithReservedBits(26, 4)
                .WithValueField(30, 2, name: "dbg_sel");

            // 0x48 CEATA - CE-ATA/SDIO mode register
            map[0x48] = new DoubleWordRegister(this)
                .WithFlag(0, name: "ata_mode")
                .WithFlag(1, name: "enable_sdio_irq")
                .WithFlag(2, name: "sdio_4wires_irq")
                .WithFlag(3, name: "sdio_4wires_multi_irq")
                .WithReservedBits(4, 28);

            // 0x54 DSR - Data Status Register
            map[0x54] = new DoubleWordRegister(this)
                .WithValueField(0, 8, FieldMode.Read, name: "sd_data_i_ll",
                    valueProviderCallback: _ => 0xFF) // all data lines high (idle)
                .WithReservedBits(8, 24);

            // 0x58 CDCR - Clock Duty Cycle Register
            map[0x58] = new DoubleWordRegister(this, 0x01)
                .WithFlag(0, name: "clk_config")
                .WithReservedBits(1, 31);

            // 0x5C CASR - Cache Status Register
            map[0x5C] = new DoubleWordRegister(this)
                .WithFlag(0, name: "sd_req")
                .WithFlag(1, name: "sd_busy",
                    valueProviderCallback: _ => false)
                .WithFlag(2, FieldMode.Read, name: "cache_busy",
                    valueProviderCallback: _ => false)
                .WithFlag(3, name: "cache_flush")
                .WithReservedBits(4, 28);

            // 0x60 CACR - Cache Control Register
            // Reset: cache_en=1, cache_to_en=1, cache_sdsc=1, cache_pref_block=8,
            //        cache_block=4, stop_has_rsp=1, stop_index=0x0C,
            //        read_has_rsp=1, read_index=0x12
            map[0x60] = new DoubleWordRegister(this, 0xD0844C52)
                .WithValueField(0, 32, name: "CACR");

            // 0x64 CACNT - Cache Counter Register
            map[0x64] = new DoubleWordRegister(this, 0xFFFF0020)
                .WithValueField(0, 32, name: "CACNT");

            // 0x68 CAOFF - Cache Offset Register
            map[0x68] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "cache_offset");

            return map;
        }

        // ── Status Register bit definitions ─────────────────────────────

        private const uint SR_CMD_BUSY       = 1 << 0;
        private const uint SR_CMD_DONE       = 1 << 1;
        private const uint SR_CMD_RSP_CRC    = 1 << 2;
        private const uint SR_CMD_TIMEOUT    = 1 << 3;
        private const uint SR_DATA_BUSY      = 1 << 4;
        private const uint SR_DATA_DONE      = 1 << 5;
        private const uint SR_DATA_CRC       = 1 << 6;
        private const uint SR_DATA_TIMEOUT   = 1 << 7;
        private const uint SR_STARTBIT_ERR   = 1 << 8;
        private const uint SR_FIFO_UNDERRUN  = 1 << 9;
        private const uint SR_FIFO_OVERRUN   = 1 << 10;
        private const uint SR_CMD_SENT       = 1 << 12;
        private const uint SR_CARD_INSERT    = 1 << 13;
        private const uint SR_CARD_REMOVE    = 1 << 14;
        private const uint SR_CARD_EXIST     = 1 << 15;
        private const uint SR_SDIO           = 1 << 16;
        private const uint SR_CACHE_ERR      = 1 << 17;

        // W1C bits (all except cmd_busy, data_busy, card_exist which are read-only)
        private const uint SR_W1C_MASK = SR_CMD_DONE | SR_CMD_RSP_CRC | SR_CMD_TIMEOUT |
            SR_DATA_DONE | SR_DATA_CRC | SR_DATA_TIMEOUT | SR_STARTBIT_ERR |
            SR_FIFO_UNDERRUN | SR_FIFO_OVERRUN | SR_CMD_SENT |
            SR_CARD_INSERT | SR_CARD_REMOVE | SR_SDIO | SR_CACHE_ERR;

        // Bits that can generate interrupts
        private const uint SR_IRQ_MASK = SR_CMD_DONE | SR_CMD_RSP_CRC | SR_CMD_TIMEOUT |
            SR_DATA_DONE | SR_DATA_CRC | SR_DATA_TIMEOUT | SR_STARTBIT_ERR |
            SR_FIFO_UNDERRUN | SR_FIFO_OVERRUN | SR_CMD_SENT |
            SR_CARD_INSERT | SR_CARD_REMOVE | SR_SDIO | SR_CACHE_ERR;

        // ── State ──────────────────────────────────────────────────────

        private readonly IMachine machine;
        private readonly DoubleWordRegisterCollection registers;
        private readonly byte[] storage;
        private readonly long cardSize;
        private readonly string imagePath;
        private readonly Queue<uint> fifo;
        private FileStream backingFile;

        private uint srValue;
        private uint ierValue;

        // Command state
        private uint cmdIndex;
        private uint cmdArg;
        private bool cmdHasRsp;
        private bool cmdLongRsp;
        private bool ccrTxEn;
        private bool ccrPend;

        // Response state
        private uint rspIndex;
        private uint rspArg1;
        private uint rspArg2;
        private uint rspArg3;
        private uint rspArg4;

        // Data transfer state
        private bool dataReadMode;
        private bool dataWriteMode;
        private long dataOffset;
        private int dataRemaining;
        private int blockSize;
        private int blocksTransferred;
        private int writeWordCount;
        private uint dataLen;

        // SD protocol state
        private bool nextIsAppCmd;
        private bool cardSelected;
        private uint cardRCA;
    }
}
