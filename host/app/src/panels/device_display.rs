use crate::state::DeviceHandle;

const DISPLAY_WIDTH: usize = 128;
const DISPLAY_HEIGHT: usize = 64;

/// Physical active area of the SSD1312 OLED panel (mm).
const ACTIVE_AREA_WIDTH_MM: f32 = 28.82;
const ACTIVE_AREA_HEIGHT_MM: f32 = 14.1;

pub fn show(ui: &mut egui::Ui, dev: &DeviceHandle) {
    match &dev.display_frame {
        Some(frame) => {
            let image = gddram_to_color_image(&frame.data);
            let texture = ui.ctx().load_texture(
                format!("display_{}", dev.id.0),
                image,
                egui::TextureOptions::NEAREST,
            );

            let available = ui.available_size();

            // Scaled-to-fit copy.
            let scale_x = available.x / DISPLAY_WIDTH as f32;
            let scale_y = (available.y * 0.5) / DISPLAY_HEIGHT as f32;
            let scale = scale_x.min(scale_y).max(1.0).floor();
            let fitted_size = egui::vec2(
                DISPLAY_WIDTH as f32 * scale,
                DISPLAY_HEIGHT as f32 * scale,
            );

            ui.vertical_centered(|ui| {
                ui.add(
                    egui::Image::from_texture(&texture).fit_to_exact_size(fitted_size),
                );

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // 1:1 physical-size copy.
                // Assume physical DPI = pixels_per_point * 96 (Windows convention).
                let ppp = ui.ctx().pixels_per_point();
                let phys_dpi = ppp * 96.0;
                let true_size = egui::vec2(
                    ACTIVE_AREA_WIDTH_MM / 25.4 * phys_dpi / ppp,
                    ACTIVE_AREA_HEIGHT_MM / 25.4 * phys_dpi / ppp,
                );

                ui.label(
                    egui::RichText::new("1 : 1").weak().small(),
                );
                ui.add(
                    egui::Image::from_texture(&texture).fit_to_exact_size(true_size),
                );
            });
        }
        None => {
            ui.centered_and_justified(|ui| {
                ui.label("No display data received yet.");
            });
        }
    }
}

fn gddram_to_color_image(data: &[u8]) -> egui::ColorImage {
    let mut pixels = vec![egui::Color32::BLACK; DISPLAY_WIDTH * DISPLAY_HEIGHT];

    for page in 0..8 {
        for col in 0..DISPLAY_WIDTH {
            let byte = data.get(page * DISPLAY_WIDTH + col).copied().unwrap_or(0);
            for bit in 0..8 {
                let y = page * 8 + bit;
                let on = (byte >> bit) & 1 != 0;
                pixels[y * DISPLAY_WIDTH + col] = if on {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::BLACK
                };
            }
        }
    }

    egui::ColorImage {
        size: [DISPLAY_WIDTH, DISPLAY_HEIGHT],
        pixels,
    }
}
