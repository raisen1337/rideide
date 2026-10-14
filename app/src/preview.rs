//! Previews for files that aren't text: images, audio, and a hex dump for everything else.

use crate::{icons, theme};
use eframe::egui::{
    self, vec2, Align, Color32, Context, Frame, Layout, Margin, RichText, ScrollArea,
    TextureHandle, TextureOptions, Ui,
};
use rodio::Source;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

/// Larger images are scaled down before upload; the GPU has a texture size limit.
const MAX_TEXTURE: u32 = 4096;
const HEX_BYTES: usize = 16 * 1024;

pub enum Preview {
    Image {
        texture: TextureHandle,
        /// The file's real dimensions, which may exceed the texture's.
        size: [u32; 2],
        /// Scale relative to the real size; `None` fits the image to the view.
        zoom: Option<f32>,
    },
    Audio(Audio),
    Binary {
        dump: String,
        size: usize,
    },
    /// One edit Claude made, as removed and added lines.
    Diff(Vec<crate::diff::Line>),
}

pub enum Loaded {
    Text(String),
    Preview(Preview),
}

pub struct Audio {
    /// Keeps the output device open for as long as the tab lives.
    _device: rodio::MixerDeviceSink,
    player: rodio::Player,
    bytes: Arc<[u8]>,
    duration: Option<Duration>,
}

fn extension(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .rsplit_once('.')
        .map_or(String::new(), |(_, e)| e.to_lowercase())
}

fn decoder(bytes: &Arc<[u8]>) -> Result<rodio::Decoder<Cursor<Arc<[u8]>>>, String> {
    rodio::Decoder::new(Cursor::new(bytes.clone())).map_err(|e| format!("can't decode audio: {e}"))
}

impl Audio {
    fn new(data: Vec<u8>) -> Result<Self, String> {
        let bytes: Arc<[u8]> = data.into();
        let source = decoder(&bytes)?;
        let duration = source.total_duration();
        let device = rodio::DeviceSinkBuilder::open_default_sink()
            .map_err(|e| format!("no audio output device: {e}"))?;
        let player = rodio::Player::connect_new(device.mixer());
        player.pause();
        player.append(source);
        Ok(Self {
            _device: device,
            player,
            bytes,
            duration,
        })
    }

    fn toggle(&mut self) {
        // Once the track has played through, the queue is empty and it has to be loaded again.
        if self.player.empty() {
            if let Ok(source) = decoder(&self.bytes) {
                self.player.append(source);
            }
            self.player.play();
        } else if self.player.is_paused() {
            self.player.play();
        } else {
            self.player.pause();
        }
    }
}

/// Decides how a file is shown. Text comes back as text; everything else as a preview.
pub fn load(ctx: &Context, path: &str, data: Vec<u8>) -> Result<Loaded, String> {
    match extension(path).as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" => {
            let image =
                image::load_from_memory(&data).map_err(|e| format!("can't decode image: {e}"))?;
            let size = [image.width(), image.height()];
            let shown = if size[0].max(size[1]) > MAX_TEXTURE {
                image.thumbnail(MAX_TEXTURE, MAX_TEXTURE)
            } else {
                image
            };
            let rgba = shown.to_rgba8();
            let pixels = egui::ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            );
            let texture = ctx.load_texture(path, pixels, TextureOptions::LINEAR);
            return Ok(Loaded::Preview(Preview::Image {
                texture,
                size,
                zoom: None,
            }));
        }
        "mp3" | "wav" | "ogg" | "oga" | "flac" | "m4a" | "aac" => {
            return Audio::new(data).map(|audio| Loaded::Preview(Preview::Audio(audio)));
        }
        _ => {}
    }

    let binary = data[..data.len().min(8192)].contains(&0);
    match String::from_utf8(data) {
        Ok(text) if !binary => Ok(Loaded::Text(text)),
        Ok(text) => Ok(Loaded::Preview(hex_dump(text.as_bytes()))),
        Err(e) => Ok(Loaded::Preview(hex_dump(e.as_bytes()))),
    }
}

fn hex_dump(data: &[u8]) -> Preview {
    let mut dump = String::new();
    for (row, chunk) in data[..data.len().min(HEX_BYTES)].chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|b| {
                if b.is_ascii_graphic() || *b == b' ' {
                    *b as char
                } else {
                    '.'
                }
            })
            .collect();
        dump.push_str(&format!(
            "{:08x}  {:<47}  {ascii}\n",
            row * 16,
            hex.join(" ")
        ));
    }
    Preview::Binary {
        dump,
        size: data.len(),
    }
}

fn clock(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn human_size(bytes: usize) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

fn round_button(ui: &mut Ui, icon: char, hint: &str) -> bool {
    let button = egui::Button::new(RichText::new(icon).size(20.0).color(Color32::WHITE))
        .fill(theme::ACCENT().gamma_multiply(0.6))
        .corner_radius(24)
        .min_size(vec2(48.0, 48.0));
    ui.add(button).on_hover_text(hint).clicked()
}

pub fn show(ui: &mut Ui, preview: &mut Preview, name: &str) {
    match preview {
        Preview::Image {
            texture,
            size,
            zoom,
        } => {
            let real = vec2(size[0] as f32, size[1] as f32);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("{} × {}", size[0], size[1]))
                        .small()
                        .color(theme::COMMENT()),
                );
                if ui.selectable_label(zoom.is_none(), "Fit").clicked() {
                    *zoom = None;
                }
                if ui.selectable_label(*zoom == Some(1.0), "100%").clicked() {
                    *zoom = Some(1.0);
                }
                let mut value = zoom.unwrap_or(1.0);
                if ui
                    .add(
                        egui::Slider::new(&mut value, 0.1..=8.0)
                            .logarithmic(true)
                            .show_value(false),
                    )
                    .changed()
                {
                    *zoom = Some(value);
                }
                if let Some(z) = zoom {
                    ui.label(
                        RichText::new(format!("{:.0}%", *z * 100.0))
                            .small()
                            .color(theme::COMMENT()),
                    );
                }
            });
            ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                let available = ui.available_size();
                let scale = match zoom {
                    Some(z) => *z,
                    // Fit, but never blow a small image up.
                    None => (available.x / real.x).min(available.y / real.y).min(1.0),
                };
                let shown = real * scale;
                // Centred while it fits; a larger image starts at the corner and scrolls.
                let pad = ((available - shown) / 2.0).max(vec2(0.0, 0.0));
                ui.add_space(pad.y);
                ui.horizontal(|ui| {
                    ui.add_space(pad.x);
                    let source = egui::load::SizedTexture::new(texture.id(), shown);
                    ui.add(egui::Image::new(source).fit_to_exact_size(shown));
                });
            });
        }
        Preview::Audio(audio) => {
            let playing = !audio.player.is_paused() && !audio.player.empty();
            if playing {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add_space((ui.available_height() * 0.28).max(10.0));
                ui.label(
                    RichText::new(icons::MUSIC)
                        .size(54.0)
                        .color(theme::ACCENT()),
                );
                ui.add_space(8.0);
                ui.label(RichText::new(name).size(17.0).strong());
                ui.add_space(14.0);
                let icon = if playing { icons::PAUSE } else { icons::PLAY };
                if round_button(ui, icon, if playing { "Pause" } else { "Play" }) {
                    audio.toggle();
                }
                ui.add_space(12.0);

                let position = if audio.player.empty() {
                    Duration::ZERO
                } else {
                    audio.player.get_pos()
                };
                Frame::NONE
                    .inner_margin(Margin::symmetric(24, 0))
                    .show(ui, |ui| {
                        ui.set_max_width(520.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(clock(position))
                                    .small()
                                    .monospace()
                                    .color(theme::COMMENT()),
                            );
                            match audio.duration {
                                Some(total) if !total.is_zero() => {
                                    let mut secs = position.as_secs_f32();
                                    ui.spacing_mut().slider_width =
                                        (ui.available_width() - 60.0).max(80.0);
                                    let slider =
                                        egui::Slider::new(&mut secs, 0.0..=total.as_secs_f32())
                                            .show_value(false);
                                    if ui.add(slider).changed() {
                                        let _ =
                                            audio.player.try_seek(Duration::from_secs_f32(secs));
                                    }
                                    ui.label(
                                        RichText::new(clock(total))
                                            .small()
                                            .monospace()
                                            .color(theme::COMMENT()),
                                    );
                                }
                                _ => {
                                    ui.label(
                                        RichText::new("length unknown")
                                            .small()
                                            .color(theme::COMMENT()),
                                    );
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(icons::VOLUME).color(theme::COMMENT()));
                            let mut volume = audio.player.volume();
                            ui.spacing_mut().slider_width = 140.0;
                            if ui
                                .add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
                                .changed()
                            {
                                audio.player.set_volume(volume);
                            }
                        });
                    });
            });
        }
        Preview::Diff(lines) => {
            ui.add_space(6.0);
            crate::diff::show(ui, lines, "diff_tab", f32::INFINITY);
        }
        Preview::Binary { dump, size } => {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let shown = if *size > HEX_BYTES {
                    format!(
                        "Binary file · {} · first {} shown",
                        human_size(*size),
                        human_size(HEX_BYTES)
                    )
                } else {
                    format!("Binary file · {}", human_size(*size))
                };
                ui.label(RichText::new(shown).small().color(theme::COMMENT()));
            });
            ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(dump.as_str())
                            .monospace()
                            .color(theme::MUTED()),
                    );
                });
            });
        }
    }
}
