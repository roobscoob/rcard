// SF32LB52 LCDC → SSD1312 128×64 OLED display — Renode peripheral model
//
// Models the SF32LB52's LCD Controller (LCDC) at 0x5000_8000 in 4-wire SPI
// mode, with an internal SSD1312 display controller receiving the SPI
// command/data stream.  Firmware interacts exclusively with the LCDC
// registers; the SPI bus and D/C# pin are modelled implicitly.
//
// Implemented paths:
//   • Configuration path — single-access writes via LCD_WR + WR_TRIG
//     (used for display init commands and small data writes)
//   • Image path — bulk DMA from LAYER0_SRC framebuffer on COMMAND.start
//     (used for full-frame updates)
//
// The display renders to a Renode video window via AutoRepaintingVideo,
// converting the 1bpp page-oriented GDDRAM to RGBX8888.  Use
// `showAnalyzer lcdc` in the .resc to open the display window.

using System;
using System.Collections.Generic;
using Antmicro.Renode.Backends.Display;
using Antmicro.Renode.Core;
using Antmicro.Renode.Core.Structure.Registers;
using Antmicro.Renode.Logging;
using Antmicro.Renode.Peripherals.Bus;
using Antmicro.Renode.Peripherals.Video;

namespace Antmicro.Renode.Peripherals.Display
{
    [AllowedTranslations(AllowedTranslation.ByteToDoubleWord | AllowedTranslation.WordToDoubleWord)]
    public class SF32LB52_LCDC : AutoRepaintingVideo, IDoubleWordPeripheral, IKnownSize
    {
        public SF32LB52_LCDC(IMachine machine) : base(machine)
        {
            ssd1312 = new SSD1312Model(this);
            sysbus = machine.GetSystemBus(this);

            Reconfigure(DisplayWidth, DisplayHeight, PixelFormat.RGBX8888);

            registers = new DoubleWordRegisterCollection(this, BuildRegisterMap());
            Reset();
        }

        // ── IDoubleWordPeripheral ──────────────────────────────────────

        public uint ReadDoubleWord(long offset)
        {
            return registers.Read(offset);
        }

        public void WriteDoubleWord(long offset, uint value)
        {
            registers.Write(offset, value);
        }

        public override void Reset()
        {
            registers.Reset();
            ssd1312.Reset();

            lcdBusy = false;
            lcdIntfSel = 0;
            spiLineMode = 0;
            spiLcdFormat = 0;
            lcdFormat = 0;
            spiClkDiv = 0;
            lcdSingleType = false;
            lcdWrData = 0;
            lcdRstb = false;
            eofRawStat = false;

            layer0Active = false;
            layer0Format = 0;
            layer0Addr = 0;
            canvasTlX = 0;
            canvasTlY = 0;
            canvasBrX = 0;
            canvasBrY = 0;
        }

        public long Size => 0x128;

        // ── Video output ───────────────────────────────────────────────

        protected override void Repaint()
        {
            ssd1312.RenderToBuffer(buffer);
        }

        // ── Register map ───────────────────────────────────────────────

        private Dictionary<long, DoubleWordRegister> BuildRegisterMap()
        {
            var map = new Dictionary<long, DoubleWordRegister>();

            // 0x00 COMMAND — reset + start trigger
            map[0x00] = new DoubleWordRegister(this)
                .WithReservedBits(2, 30)
                .WithFlag(1, FieldMode.Write, name: "RESET",
                    writeCallback: (_, val) =>
                    {
                        if(val)
                        {
                            this.Log(LogLevel.Debug, "LCDC: graphics reset");
                        }
                    })
                .WithFlag(0, FieldMode.Write, name: "START",
                    writeCallback: (_, val) =>
                    {
                        if(val)
                        {
                            OnStartTriggered();
                        }
                    });

            // 0x04 STATUS
            map[0x04] = new DoubleWordRegister(this)
                .WithReservedBits(3, 29)
                .WithTaggedFlag("JDI_PAR_RUN", 2)
                .WithTaggedFlag("DPI_RUN", 1)
                .WithFlag(0, FieldMode.Read, name: "LCD_BUSY",
                    valueProviderCallback: _ => lcdBusy);

            // 0x08 IRQ — raw + masked interrupt status
            map[0x08] = new DoubleWordRegister(this)
                .WithReservedBits(23, 9)
                .WithTaggedFlag("LINE_DONE_RAW_STAT", 22)
                .WithTaggedFlag("JDI_PAR_UDR_RAW_STAT", 21)
                .WithTaggedFlag("JDI_PARL_INTR_RAW_STAT", 20)
                .WithTaggedFlag("DPI_UDR_RAW_STAT", 19)
                .WithTaggedFlag("DPIL_INTR_RAW_STAT", 18)
                .WithTaggedFlag("ICB_OF_RAW_STAT", 17)
                .WithFlag(16, FieldMode.Read | FieldMode.WriteOneToClear, name: "EOF_RAW_STAT",
                    valueProviderCallback: _ => eofRawStat,
                    writeCallback: (_, val) => { if(val) eofRawStat = false; })
                .WithReservedBits(7, 9)
                .WithTaggedFlag("LINE_DONE_STAT", 6)
                .WithTaggedFlag("JDI_PAR_UDR_STAT", 5)
                .WithTaggedFlag("JDI_PARL_INTR_STAT", 4)
                .WithTaggedFlag("DPI_UDR_STAT", 3)
                .WithTaggedFlag("DPIL_INTR_STAT", 2)
                .WithTaggedFlag("ICB_OF_STAT", 1)
                .WithFlag(0, FieldMode.Read, name: "EOF_STAT",
                    valueProviderCallback: _ => eofRawStat && eofMask);

            // 0x0C SETTING — interrupt masks, line done number
            map[0x0C] = new DoubleWordRegister(this)
                .WithReservedBits(27, 5)
                .WithValueField(16, 11, name: "LINE_DONE_NUM")
                .WithReservedBits(9, 7)
                .WithTaggedFlag("AUTO_GATE_EN", 8)
                .WithReservedBits(7, 1)
                .WithTaggedFlag("LINE_DONE_MASK", 6)
                .WithTaggedFlag("JDI_PAR_UDR_MASK", 5)
                .WithTaggedFlag("JDI_PARL_INTR_MASK", 4)
                .WithTaggedFlag("DPI_UDR_MASK", 3)
                .WithTaggedFlag("DPIL_INTR_MASK", 2)
                .WithTaggedFlag("ICB_OF_MASK", 1)
                .WithFlag(0, name: "EOF_MASK",
                    writeCallback: (_, val) => eofMask = val,
                    valueProviderCallback: _ => eofMask);

            // 0x10 CANVAS_TL_POS
            map[0x10] = new DoubleWordRegister(this)
                .WithReservedBits(27, 5)
                .WithValueField(16, 11, name: "Y0",
                    writeCallback: (_, val) => canvasTlY = (int)val,
                    valueProviderCallback: _ => (uint)canvasTlY)
                .WithReservedBits(11, 5)
                .WithValueField(0, 11, name: "X0",
                    writeCallback: (_, val) => canvasTlX = (int)val,
                    valueProviderCallback: _ => (uint)canvasTlX);

            // 0x14 CANVAS_BR_POS
            map[0x14] = new DoubleWordRegister(this)
                .WithReservedBits(27, 5)
                .WithValueField(16, 11, name: "Y1",
                    writeCallback: (_, val) => canvasBrY = (int)val,
                    valueProviderCallback: _ => (uint)canvasBrY)
                .WithReservedBits(11, 5)
                .WithValueField(0, 11, name: "X1",
                    writeCallback: (_, val) => canvasBrX = (int)val,
                    valueProviderCallback: _ => (uint)canvasBrX);

            // 0x18 CANVAS_BG
            map[0x18] = new DoubleWordRegister(this)
                .WithReservedBits(28, 4)
                .WithTaggedFlag("H_MIRROR", 27)
                .WithTaggedFlag("LB_BYPASS", 26)
                .WithTaggedFlag("ALL_BLENDING_BYPASS", 25)
                .WithTaggedFlag("BG_BLENDING_BYPASS", 24)
                .WithValueField(16, 8, name: "RED")
                .WithValueField(8, 8, name: "GREEN")
                .WithValueField(0, 8, name: "BLUE");

            // 0x1C LAYER0_CONFIG
            map[0x1C] = new DoubleWordRegister(this)
                .WithReservedBits(31, 1)
                .WithTaggedFlag("V_MIRROR", 30)
                .WithTaggedFlag("ALPHA_BLEND", 29)
                .WithFlag(28, name: "ACTIVE",
                    writeCallback: (_, val) => layer0Active = val,
                    valueProviderCallback: _ => layer0Active)
                .WithTaggedFlag("LINE_FETCH_MODE", 27)
                .WithTaggedFlag("PREFETCH_EN", 26)
                .WithValueField(13, 13, name: "WIDTH")
                .WithTaggedFlag("FILTER_EN", 12)
                .WithValueField(4, 8, name: "ALPHA")
                .WithTaggedFlag("ALPHA_SEL", 3)
                .WithValueField(0, 3, name: "FORMAT",
                    writeCallback: (_, val) => layer0Format = (int)val,
                    valueProviderCallback: _ => (uint)layer0Format);

            // 0x20 LAYER0_TL_POS
            map[0x20] = new DoubleWordRegister(this)
                .WithReservedBits(27, 5)
                .WithValueField(16, 11, name: "Y0")
                .WithReservedBits(11, 5)
                .WithValueField(0, 11, name: "X0");

            // 0x24 LAYER0_BR_POS
            map[0x24] = new DoubleWordRegister(this)
                .WithReservedBits(27, 5)
                .WithValueField(16, 11, name: "Y1")
                .WithReservedBits(11, 5)
                .WithValueField(0, 11, name: "X1");

            // 0x28 LAYER0_FILTER
            map[0x28] = new DoubleWordRegister(this, 0)
                .WithValueField(24, 8, name: "FILTER_MASK")
                .WithValueField(16, 8, name: "FILTER_R")
                .WithValueField(8, 8, name: "FILTER_G")
                .WithValueField(0, 8, name: "FILTER_B");

            // 0x2C LAYER0_SRC — framebuffer address
            map[0x2C] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "ADDR",
                    writeCallback: (_, val) => layer0Addr = (uint)val,
                    valueProviderCallback: _ => layer0Addr);

            // 0x30 LAYER0_FILL
            map[0x30] = new DoubleWordRegister(this)
                .WithReservedBits(26, 6)
                .WithTaggedFlag("ENDIAN", 25)
                .WithTaggedFlag("BG_MODE", 24)
                .WithValueField(16, 8, name: "BG_R")
                .WithValueField(8, 8, name: "BG_G")
                .WithValueField(0, 8, name: "BG_B");

            // 0x80 LCD_CONF — interface selection, format
            map[0x80] = new DoubleWordRegister(this)
                .WithReservedBits(21, 11)
                .WithValueField(19, 2, name: "SPI_RD_SEL")
                .WithTaggedFlag("ENDIAN", 18)
                .WithTaggedFlag("DIRECT_INTF_EN", 17)
                .WithValueField(15, 2, name: "JDI_SER_FORMAT")
                .WithValueField(12, 3, name: "DPI_LCD_FORMAT")
                .WithValueField(10, 2, name: "SPI_LCD_FORMAT",
                    writeCallback: (_, val) => spiLcdFormat = (int)val,
                    valueProviderCallback: _ => (uint)spiLcdFormat)
                .WithValueField(8, 2, name: "AHB_FORMAT")
                .WithValueField(5, 3, name: "LCD_FORMAT",
                    writeCallback: (_, val) => lcdFormat = (int)val,
                    valueProviderCallback: _ => (uint)lcdFormat)
                .WithValueField(2, 3, name: "LCD_INTF_SEL",
                    writeCallback: (_, val) =>
                    {
                        lcdIntfSel = (int)val;
                        this.Log(LogLevel.Debug, "LCDC: interface select = {0}", lcdIntfSel);
                    },
                    valueProviderCallback: _ => (uint)lcdIntfSel)
                .WithValueField(0, 2, name: "TARGET_LCD");

            // 0x84 LCD_IF_CONF — SPI timing, polarities, reset pin
            map[0x84] = new DoubleWordRegister(this)
                .WithReservedBits(26, 6)
                .WithTaggedFlag("CTRL_DLY_SET", 25)
                .WithTaggedFlag("DO_DLY_SET", 24)
                .WithFlag(23, name: "LCD_RSTB",
                    writeCallback: (_, val) =>
                    {
                        lcdRstb = val;
                        this.Log(LogLevel.Debug, "LCDC: LCD_RSTB = {0}", val);
                        if(!val)
                        {
                            ssd1312.Reset();
                        }
                    },
                    valueProviderCallback: _ => lcdRstb)
                .WithTaggedFlag("RD_POL", 22)
                .WithTaggedFlag("WR_POL", 21)
                .WithTaggedFlag("RS_POL", 20)
                .WithTaggedFlag("CS1_POL", 19)
                .WithTaggedFlag("CS0_POL", 18)
                .WithValueField(12, 6, name: "PWH")
                .WithValueField(6, 6, name: "PWL")
                .WithValueField(3, 3, name: "TAH")
                .WithValueField(0, 3, name: "TAS");

            // 0x88 LCD_MEM
            map[0x88] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "ADDR");

            // 0x8C LCD_O_WIDTH
            map[0x8C] = new DoubleWordRegister(this)
                .WithReservedBits(16, 16)
                .WithValueField(0, 16, name: "OFFSET");

            // 0x90 LCD_SINGLE — command/data type, triggers, busy
            map[0x90] = new DoubleWordRegister(this)
                .WithReservedBits(4, 28)
                .WithFlag(3, FieldMode.Read, name: "LCD_BUSY",
                    valueProviderCallback: _ => false)
                .WithFlag(2, FieldMode.Write, name: "RD_TRIG",
                    writeCallback: (_, val) =>
                    {
                        if(val)
                        {
                            this.Log(LogLevel.Debug, "LCDC: SPI read trigger (not implemented)");
                        }
                    })
                .WithFlag(1, FieldMode.Write, name: "WR_TRIG",
                    writeCallback: (_, val) =>
                    {
                        if(val)
                        {
                            OnSingleWriteTriggered();
                        }
                    })
                .WithFlag(0, name: "TYPE",
                    writeCallback: (_, val) => lcdSingleType = val,
                    valueProviderCallback: _ => lcdSingleType);

            // 0x94 LCD_WR — data to send over SPI
            map[0x94] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "DATA",
                    writeCallback: (_, val) => lcdWrData = (uint)val,
                    valueProviderCallback: _ => lcdWrData);

            // 0x98 LCD_RD — data read back from SPI
            map[0x98] = new DoubleWordRegister(this)
                .WithValueField(0, 32, FieldMode.Read, name: "DATA",
                    valueProviderCallback: _ => 0);

            // 0x9C SPI_IF_CONF — SPI clock, CS, line mode, etc.
            map[0x9C] = new DoubleWordRegister(this, 0x08000A00)
                .WithReservedBits(31, 1)
                .WithTaggedFlag("SPI_CLK_INIT", 30)
                .WithTaggedFlag("SPI_CLK_POL", 29)
                .WithTaggedFlag("SPI_CS_POL", 28)
                .WithFlag(27, name: "SPI_CS_AUTO_DIS")
                .WithTaggedFlag("SPI_CS_NO_IDLE", 26)
                .WithTaggedFlag("SPI_CLK_AUTO_DIS", 25)
                .WithTaggedFlag("SPI_RD_MODE", 24)
                .WithValueField(22, 2, name: "WR_LEN")
                .WithValueField(20, 2, name: "RD_LEN")
                .WithValueField(17, 3, name: "LINE",
                    writeCallback: (_, val) =>
                    {
                        spiLineMode = (int)val;
                        this.Log(LogLevel.Debug, "LCDC: SPI line mode = {0}", spiLineMode);
                    },
                    valueProviderCallback: _ => (uint)spiLineMode)
                .WithValueField(14, 3, name: "DUMMY_CYCLE")
                .WithValueField(6, 8, name: "CLK_DIV",
                    writeCallback: (_, val) => spiClkDiv = (int)val,
                    valueProviderCallback: _ => (uint)spiClkDiv)
                .WithValueField(0, 6, name: "WAIT_CYCLE");

            // 0xA0 TE_CONF
            map[0xA0] = new DoubleWordRegister(this)
                .WithReservedBits(21, 11)
                .WithTaggedFlag("FMARK_SOURCE", 20)
                .WithTaggedFlag("FMARK_MODE", 19)
                .WithValueField(3, 16, name: "VSYNC_DET_CNT")
                .WithTaggedFlag("MODE", 2)
                .WithTaggedFlag("FMARK_POL", 1)
                .WithTaggedFlag("ENABLE", 0);

            // 0xA4 TE_CONF2
            map[0xA4] = new DoubleWordRegister(this)
                .WithValueField(0, 32, name: "DLY_CNT");

            return map;
        }

        // ── Configuration path: single-access SPI write ────────────────

        private void OnSingleWriteTriggered()
        {
            bool isData = lcdSingleType;

            uint wrLen = (registers.Read(0x9C) >> 22) & 0x3;
            int byteCount = (int)wrLen + 1;

            for(int i = 0; i < byteCount; i++)
            {
                byte b = (byte)((lcdWrData >> (i * 8)) & 0xFF);

                if(isData)
                {
                    ssd1312.WriteData(b);
                }
                else
                {
                    ssd1312.WriteCommand(b);
                }
            }
        }

        // ── Image path: bulk DMA transfer ──────────────────────────────

        private void OnStartTriggered()
        {
            this.Log(LogLevel.Debug, "LCDC: frame start triggered");

            if(!layer0Active)
            {
                this.Log(LogLevel.Debug, "LCDC: layer0 not active, sending background only");
                eofRawStat = true;
                return;
            }

            int width = canvasBrX - canvasTlX + 1;
            int height = canvasBrY - canvasTlY + 1;

            if(width <= 0 || height <= 0)
            {
                this.Log(LogLevel.Warning, "LCDC: invalid canvas dimensions {0}x{1}", width, height);
                eofRawStat = true;
                return;
            }

            int pixelCount = width * height;
            int bytesPerPixel = GetBytesPerPixel(layer0Format);
            int totalBytes = pixelCount * bytesPerPixel;

            this.Log(LogLevel.Debug,
                "LCDC: DMA {0}x{1} pixels, format={2}, bpp={3}, src=0x{4:X8}",
                width, height, layer0Format, bytesPerPixel, layer0Addr);

            for(int i = 0; i < totalBytes; i++)
            {
                byte pixel = sysbus.ReadByte(layer0Addr + (uint)i);
                ssd1312.WriteData(pixel);
            }

            eofRawStat = true;
            this.Log(LogLevel.Debug, "LCDC: frame complete, EOF set");
        }

        private int GetBytesPerPixel(int format)
        {
            switch(format)
            {
                case 0: return 2; // RGB565
                case 1: return 3; // RGB888
                case 2: return 4; // ARGB8888
                case 3: return 3; // ARGB8565
                case 4: return 1; // RGB332
                case 5: return 1; // A8
                case 6: return 1; // L8
                default: return 2;
            }
        }

        // ── SSD1312 display inspection (forwarded from inner model) ────

        public string DumpGDDRAM() => ssd1312.DumpGDDRAM();
        public byte ReadGDDRAM(int page, int column) => ssd1312.ReadGDDRAM(page, column);
        public bool DisplayOn => ssd1312.DisplayOn;
        public bool ChargePumpEnabled => ssd1312.ChargePumpEnabled;
        public byte DisplayContrast => ssd1312.Contrast;

        // ── Private state ──────────────────────────────────────────────

        private readonly IBusController sysbus;
        private readonly DoubleWordRegisterCollection registers;
        private readonly SSD1312Model ssd1312;

        private bool lcdBusy;
        private int lcdIntfSel;
        private int spiLineMode;
        private int spiLcdFormat;
        private int lcdFormat;
        private int spiClkDiv;
        private bool lcdSingleType;
        private uint lcdWrData;
        private bool lcdRstb;
        private bool eofRawStat;
        private bool eofMask;

        private bool layer0Active;
        private int layer0Format;
        private uint layer0Addr;
        private int canvasTlX, canvasTlY;
        private int canvasBrX, canvasBrY;

        private const int DisplayWidth = 128;
        private const int DisplayHeight = 64;

        // ================================================================
        //  SSD1312 display controller model (SSD1306-compatible)
        // ================================================================

        private class SSD1312Model
        {
            private readonly SF32LB52_LCDC parent;

            public SSD1312Model(SF32LB52_LCDC parent)
            {
                this.parent = parent;
                gddram = new byte[Pages * Width];
                pendingArgs = new List<byte>();
                Reset();
            }

            public void Reset()
            {
                displayOn = false;
                addressingMode = AddressingMode.Page;
                contrastLevel = 0x7F;
                entireDisplayOn = false;
                invertDisplay = false;
                segmentRemap = false;
                comScanReversed = false;
                muxRatio = 63;
                displayOffset = 0;
                displayStartLine = 0;
                comPinConfig = 0x12;
                clockDivide = 0x80;
                preChargePeriod = 0x22;
                vcomhDeselect = 0x20;
                chargePumpEnabled = false;

                pageAddress = 0;
                columnAddress = 0;
                columnStart = 0;
                columnEnd = Width - 1;
                pageStart = 0;
                pageEnd = Pages - 1;

                pendingCommand = null;
                pendingArgs.Clear();
                expectedArgs = 0;

                Array.Clear(gddram, 0, gddram.Length);
            }

            // ── Render GDDRAM → RGBX8888 buffer for video output ───────

            public void RenderToBuffer(byte[] buf)
            {
                // GDDRAM layout: 8 pages × 128 columns
                // Each byte holds 8 vertical pixels, LSB = topmost pixel
                // RGBX8888: 4 bytes per pixel (R, G, B, X)
                //
                // OLED colors: pixel ON = white on black background
                // Respects: displayOn, entireDisplayOn, invertDisplay,
                //           segmentRemap, comScanReversed,
                //           displayStartLine, displayOffset

                for(int y = 0; y < Height; y++)
                {
                    // Apply COM scan direction, then display offset and start line
                    int srcY = comScanReversed ? (Height - 1 - y) : y;
                    srcY = (srcY + displayOffset + displayStartLine) % Height;
                    int page = srcY / 8;
                    int bit = srcY % 8;

                    for(int x = 0; x < Width; x++)
                    {
                        // Apply segment remap
                        int srcX = segmentRemap ? (Width - 1 - x) : x;

                        bool pixelOn;
                        if(!displayOn)
                        {
                            pixelOn = false;
                        }
                        else if(entireDisplayOn)
                        {
                            pixelOn = true;
                        }
                        else
                        {
                            pixelOn = ((gddram[page * Width + srcX] >> bit) & 1) != 0;
                        }

                        if(invertDisplay)
                        {
                            pixelOn = !pixelOn;
                        }

                        // RGBX8888 as 0xRRGGBBXX → little-endian: [XX, BB, GG, RR]
                        int offset = (y * Width + x) * 4;
                        byte color = pixelOn ? (byte)0xFF : (byte)0x00;
                        buf[offset + 0] = 0x00;  // X (padding)
                        buf[offset + 1] = color; // B
                        buf[offset + 2] = color; // G
                        buf[offset + 3] = color; // R
                    }
                }
            }

            // ── SPI data byte (D/C# = 1) ──────────────────────────────

            public void WriteData(byte value)
            {
                if(pageAddress < Pages && columnAddress < Width)
                {
                    gddram[pageAddress * Width + columnAddress] = value;
                }

                switch(addressingMode)
                {
                    case AddressingMode.Page:
                        columnAddress++;
                        if(columnAddress > columnEnd)
                        {
                            columnAddress = columnStart;
                        }
                        break;

                    case AddressingMode.Horizontal:
                        columnAddress++;
                        if(columnAddress > columnEnd)
                        {
                            columnAddress = columnStart;
                            pageAddress++;
                            if(pageAddress > pageEnd)
                            {
                                pageAddress = pageStart;
                            }
                        }
                        break;

                    case AddressingMode.Vertical:
                        pageAddress++;
                        if(pageAddress > pageEnd)
                        {
                            pageAddress = pageStart;
                            columnAddress++;
                            if(columnAddress > columnEnd)
                            {
                                columnAddress = columnStart;
                            }
                        }
                        break;
                }
            }

            // ── SPI command byte (D/C# = 0) ───────────────────────────

            public void WriteCommand(byte b)
            {
                if(pendingCommand.HasValue)
                {
                    pendingArgs.Add(b);
                    if(pendingArgs.Count >= expectedArgs)
                    {
                        ExecuteCommand(pendingCommand.Value, pendingArgs);
                        pendingCommand = null;
                        pendingArgs.Clear();
                    }
                    return;
                }

                if(b <= 0x0F)
                {
                    columnAddress = (columnAddress & 0xF0) | (b & 0x0F);
                    return;
                }

                if(b >= 0x10 && b <= 0x1F)
                {
                    columnAddress = (columnAddress & 0x0F) | ((b & 0x0F) << 4);
                    return;
                }

                if(b >= 0x40 && b <= 0x7F)
                {
                    displayStartLine = b & 0x3F;
                    return;
                }

                if(b >= 0xB0 && b <= 0xB7)
                {
                    pageAddress = b & 0x07;
                    return;
                }

                switch(b)
                {
                    case 0x81: Expect(b, 1); break;
                    case 0xA4: entireDisplayOn = false; break;
                    case 0xA5: entireDisplayOn = true; break;
                    case 0xA6: invertDisplay = false; break;
                    case 0xA7: invertDisplay = true; break;
                    case 0xAE:
                        displayOn = false;
                        parent.Log(LogLevel.Info, "SSD1312: Display OFF");
                        break;
                    case 0xAF:
                        displayOn = true;
                        parent.Log(LogLevel.Info, "SSD1312: Display ON");
                        break;

                    case 0x20: Expect(b, 1); break;
                    case 0x21: Expect(b, 2); break;
                    case 0x22: Expect(b, 2); break;

                    case 0xA0: segmentRemap = false; break;
                    case 0xA1: segmentRemap = true; break;
                    case 0xA8: Expect(b, 1); break;
                    case 0xC0: comScanReversed = false; break;
                    case 0xC8: comScanReversed = true; break;
                    case 0xD3: Expect(b, 1); break;
                    case 0xDA: Expect(b, 1); break;

                    case 0xD5: Expect(b, 1); break;
                    case 0xD9: Expect(b, 1); break;
                    case 0xDB: Expect(b, 1); break;

                    case 0x8D: Expect(b, 1); break;

                    case 0x26: Expect(b, 6); break;
                    case 0x27: Expect(b, 6); break;
                    case 0x29: Expect(b, 5); break;
                    case 0x2A: Expect(b, 5); break;
                    case 0x2E: break;
                    case 0x2F: break;
                    case 0xA3: Expect(b, 2); break;

                    case 0xE3: break;

                    case 0xAD: Expect(b, 1); break;

                    default:
                        parent.Log(LogLevel.Warning, "SSD1312: unknown command 0x{0:X2}", b);
                        break;
                }
            }

            private void Expect(byte cmd, int argCount)
            {
                pendingCommand = cmd;
                pendingArgs.Clear();
                expectedArgs = argCount;
            }

            private void ExecuteCommand(byte cmd, List<byte> args)
            {
                switch(cmd)
                {
                    case 0x81:
                        contrastLevel = args[0];
                        parent.Log(LogLevel.Debug, "SSD1312: contrast = 0x{0:X2}", contrastLevel);
                        break;
                    case 0x20:
                        // SSD1312 modes: 01h = Page, 02h = Page (alt), 09h = SEG-Page Horizontal
                        // SSD1306 compat: 00h = Horizontal, 01h = Vertical, 02h = Page
                        var raw = args[0] & 0x0F;
                        if(raw == 0x09)
                        {
                            addressingMode = AddressingMode.Horizontal;
                        }
                        else if(raw == 0x00)
                        {
                            addressingMode = AddressingMode.Horizontal;
                        }
                        else if(raw == 0x01)
                        {
                            addressingMode = AddressingMode.Vertical;
                        }
                        else
                        {
                            addressingMode = AddressingMode.Page;
                        }
                        parent.Log(LogLevel.Debug, "SSD1312: addressing = {0} (raw 0x{1:X2})", addressingMode, raw);
                        break;
                    case 0x21:
                        columnStart = args[0] & 0x7F;
                        columnEnd = args[1] & 0x7F;
                        columnAddress = columnStart;
                        break;
                    case 0x22:
                        pageStart = args[0] & 0x07;
                        pageEnd = args[1] & 0x07;
                        pageAddress = pageStart;
                        break;
                    case 0xA8:
                        muxRatio = args[0] & 0x3F;
                        break;
                    case 0xD3:
                        displayOffset = args[0] & 0x3F;
                        break;
                    case 0xDA:
                        comPinConfig = args[0];
                        break;
                    case 0xD5:
                        clockDivide = args[0];
                        break;
                    case 0xD9:
                        preChargePeriod = args[0];
                        break;
                    case 0xDB:
                        vcomhDeselect = args[0];
                        break;
                    case 0x8D:
                        // SSD1312 enable = 0x12 (bit 1), not SSD1306's 0x14 (bit 2)
                        chargePumpEnabled = (args[0] & 0x02) != 0;
                        parent.Log(LogLevel.Debug, "SSD1312: charge pump {0}",
                            chargePumpEnabled ? "ON" : "OFF");
                        break;
                    case 0xAD:
                        parent.Log(LogLevel.Debug, "SSD1312: pipeline clock = 0x{0:X2}", args[0]);
                        break;
                    case 0x26: case 0x27: case 0x29: case 0x2A: case 0xA3:
                        break;
                    default:
                        parent.Log(LogLevel.Warning, "SSD1312: unhandled multi-byte cmd 0x{0:X2}", cmd);
                        break;
                }
            }

            // ── Inspection ─────────────────────────────────────────────

            public string DumpGDDRAM()
            {
                return BitConverter.ToString(gddram).Replace("-", "");
            }

            public byte ReadGDDRAM(int page, int column)
            {
                if(page < 0 || page >= Pages || column < 0 || column >= Width)
                {
                    return 0;
                }
                return gddram[page * Width + column];
            }

            public bool DisplayOn => displayOn;
            public bool ChargePumpEnabled => chargePumpEnabled;
            public byte Contrast => contrastLevel;

            // ── State ──────────────────────────────────────────────────

            private const int Width = 128;
            private const int Height = 64;
            private const int Pages = Height / 8;

            private readonly byte[] gddram;

            private bool displayOn;
            private AddressingMode addressingMode;
            private byte contrastLevel;
            private bool entireDisplayOn;
            private bool invertDisplay;
            private bool segmentRemap;
            private bool comScanReversed;
            private int muxRatio;
            private int displayOffset;
            private int displayStartLine;
            private byte comPinConfig;
            private byte clockDivide;
            private byte preChargePeriod;
            private byte vcomhDeselect;
            private bool chargePumpEnabled;

            private int pageAddress;
            private int columnAddress;
            private int columnStart;
            private int columnEnd;
            private int pageStart;
            private int pageEnd;

            private byte? pendingCommand;
            private List<byte> pendingArgs;
            private int expectedArgs;

            private enum AddressingMode
            {
                Horizontal = 0,
                Vertical = 1,
                Page = 2,
            }
        }
    }
}
