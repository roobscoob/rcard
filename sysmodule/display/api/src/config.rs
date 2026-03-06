use crate::DisplayOpenError;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize, hubpack::SerializedSize)]
pub struct DisplayConfiguration {
    /// Display width in pixels. Fixed by the display model (e.g. 128 for
    /// SSD1312).
    pub width: u8,
    /// Display height in pixels. Fixed by the display model (e.g. 64 for
    /// SSD1312). Sets the MUX ratio to `height - 1`.
    pub height: u8,
    /// Contrast level (0x00–0xFF). Controls OLED segment output current and
    /// therefore brightness. 0x7F is the SSD1312 default.
    pub contrast: u8,
    /// Segment remap (SSD1312 cmd 0xA0/0xA1). When true, column address 127
    /// is mapped to SEG0, flipping the display horizontally. Set based on how
    /// the display is mounted on the PCB.
    pub segment_remap: bool,
    /// COM output scan direction (SSD1312 cmd 0xC0/0xC8). When true, scans
    /// from COM[N-1] to COM0, flipping the display vertically. Set based on
    /// how the display is mounted on the PCB.
    pub com_reversed: bool,
    /// COM pins hardware configuration byte (SSD1312 cmd 0xDA). Controls
    /// sequential vs. alternative pin mapping and left/right remap. Depends
    /// on the display panel's internal wiring — typically 0x12 for 128x64.
    pub com_pin_config: u8,
    /// Enable the internal charge pump (SSD1312 cmd 0x8D). Required when the
    /// board does not supply an external VBAT. Most modules need this enabled.
    pub charge_pump: bool,
    /// Invert all pixels (SSD1312 cmd 0xA6/0xA7). When true, lit pixels
    /// become dark and vice versa.
    pub invert: bool,
}

impl DisplayConfiguration {
    /// Start building a configuration for a display with the given resolution.
    ///
    /// Returns a builder pre-filled with sensible defaults:
    /// - contrast 0x7F (SSD1312 default)
    /// - no segment remap, no COM reversal
    /// - COM pin config 0x12 (alternative, no remap — typical for 128x64)
    /// - charge pump enabled (most modules need this)
    /// - normal (non-inverted) display
    pub fn builder(width: u8, height: u8) -> DisplayConfigurationBuilder {
        DisplayConfigurationBuilder {
            width,
            height,
            contrast: 0x7F,
            segment_remap: false,
            com_reversed: false,
            com_pin_config: 0x12,
            charge_pump: true,
            invert: false,
        }
    }

    pub fn open<S: super::display_client::DisplayServer>(
        self,
    ) -> Result<Result<super::display_client::DisplayHandle<S>, DisplayOpenError>, ipc::Error> {
        super::display_client::DisplayHandle::<S>::open(self)
    }
}

pub struct DisplayConfigurationBuilder {
    width: u8,
    height: u8,
    contrast: u8,
    segment_remap: bool,
    com_reversed: bool,
    com_pin_config: u8,
    charge_pump: bool,
    invert: bool,
}

impl DisplayConfigurationBuilder {
    /// Set the contrast level (0x00–0xFF).
    pub fn contrast(mut self, value: u8) -> Self {
        self.contrast = value;
        self
    }

    /// Flip the display horizontally (segment remap).
    pub fn flip_horizontal(mut self) -> Self {
        self.segment_remap = true;
        self
    }

    /// Flip the display vertically (COM scan direction reversed).
    pub fn flip_vertical(mut self) -> Self {
        self.com_reversed = true;
        self
    }

    /// Flip the display both horizontally and vertically (180° rotation).
    pub fn rotate_180(mut self) -> Self {
        self.segment_remap = true;
        self.com_reversed = true;
        self
    }

    /// Set the COM pins hardware configuration byte (cmd 0xDA).
    pub fn com_pin_config(mut self, value: u8) -> Self {
        self.com_pin_config = value;
        self
    }

    /// Disable the internal charge pump. Only use this if the board supplies
    /// external VBAT.
    pub fn no_charge_pump(mut self) -> Self {
        self.charge_pump = false;
        self
    }

    /// Invert all pixels (lit becomes dark, dark becomes lit).
    pub fn invert(mut self) -> Self {
        self.invert = true;
        self
    }

    /// Build the final configuration.
    pub fn build(self) -> DisplayConfiguration {
        DisplayConfiguration {
            width: self.width,
            height: self.height,
            contrast: self.contrast,
            segment_remap: self.segment_remap,
            com_reversed: self.com_reversed,
            com_pin_config: self.com_pin_config,
            charge_pump: self.charge_pump,
            invert: self.invert,
        }
    }
}
