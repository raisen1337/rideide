//! A look at a file without leaving rdrop: pictures, sound with a player, code and text in
//! colour, logs that follow what is written to them, and the bytes of anything else. Video,
//! documents and archives open in the app Windows has for them.

use crate::highlight::{self, Kind as Token, State};
use crate::{icons, theme, ui};
use eframe::egui::{
    self, text::LayoutJob, vec2, Align, Align2, Color32, Context, FontId, Frame, Key, Layout,
    Margin, RichText, ScrollArea, Sense, Stroke, TextFormat, TextureHandle, TextureOptions, Ui,
};
use rodio::Source;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

const MAX_TEXTURE: u32 = 4096;
const HEX_BYTES: usize = 64 * 1024;
const TEXT_BYTES: u64 = 4 * 1024 * 1024;
/// What a log's live view keeps of its end.
pub const TAIL_BYTES: u64 = 256 * 1024;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Image,
    Svg,
    Audio,
    Video,
    Document,
    Archive,
    /// Anything else: text when it reads as text, its bytes otherwise.
    Other,
}

fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map_or(String::new(), |(_, e)| e.to_lowercase())
}

pub fn kind_of(name: &str) -> Kind {
    match extension(name).as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff" => Kind::Image,
        "svg" => Kind::Svg,
        "mp3" | "wav" | "ogg" | "oga" | "flac" | "m4a" | "aac" | "opus" => Kind::Audio,
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "wmv" | "flv" | "m4v" => Kind::Video,
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "psd"
        | "exe" | "msi" | "dll" => Kind::Document,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "iso" => Kind::Archive,
        _ => Kind::Other,
    }
}

/// How much of a file is read to show it; nothing for what opens elsewhere.
pub fn read_limit(kind: Kind) -> u64 {
    match kind {
        Kind::Image | Kind::Svg => 64 * 1024 * 1024,
        Kind::Audio => 300 * 1024 * 1024,
        Kind::Other => TEXT_BYTES,
        Kind::Video | Kind::Document | Kind::Archive => 0,
    }
}

/// Whether a file is a log, whose end is what matters and which grows while you look.
pub fn is_log(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".log")
        || lower.ends_with(".out")
        || lower.ends_with(".err")
        || lower.contains(".log.")
}

pub struct Audio {
    _device: rodio::MixerDeviceSink,
    player: rodio::Player,
    bytes: Arc<[u8]>,
    duration: Option<Duration>,
}

fn decoder(bytes: &Arc<[u8]>) -> Result<rodio::Decoder<Cursor<Arc<[u8]>>>, String> {
    rodio::Decoder::new(Cursor::new(bytes.clone()))
        .map_err(|e| format!("This sound can't be played: {e}"))
}

impl Audio {
    fn new(data: Vec<u8>) -> Result<Self, String> {
        let bytes: Arc<[u8]> = data.into();
        let source = decoder(&bytes)?;
        let duration = source.total_duration();
        let device = rodio::DeviceSinkBuilder::open_default_sink()
            .map_err(|e| format!("No sound output: {e}"))?;
        let player = rodio::Player::connect_new(device.mixer());
        player.append(source);
        // It starts playing at once, as a player does when a file is opened in it.
        player.play();
        Ok(Self {
            _device: device,
            player,
            bytes,
            duration,
        })
    }

    fn playing(&self) -> bool {
        !self.player.is_paused() && !self.player.empty()
    }

    pub fn toggle(&mut self) {
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

pub enum Content {
    Image {
        texture: TextureHandle,
        size: [u32; 2],
        zoom: Option<f32>,
    },
    Audio(Audio),
    Text {
        lines: Vec<String>,
        tokens: Vec<Vec<(usize, Token)>>,
        truncated: bool,
    },
    Hex {
        dump: String,
    },
    /// Shown by its app, not here.
    Elsewhere(&'static str),
}

pub struct Preview {
    pub pane: u64,
    pub name: String,
    /// Where it is: a path here or on the server.
    pub path: String,
    pub place: String,
    pub size: Option<u64>,
    pub kind: Kind,
    pub content: Option<Result<Content, String>>,
    /// Following the end of a log as it grows.
    pub live: bool,
    pub asked_at: f64,
}

/// What the preview's buttons and keys asked for.
#[derive(PartialEq)]
pub enum Act {
    None,
    Close,
    Next,
    Previous,
    OpenElsewhere,
    Live(bool),
}

impl Preview {
    pub fn new(pane: u64, name: &str, path: &str, place: &str, size: Option<u64>) -> Self {
        let kind = kind_of(name);
        let content = match kind {
            Kind::Video => Some(Ok(Content::Elsewhere("Videos play in your video player."))),
            Kind::Document => Some(Ok(Content::Elsewhere(
                "This opens in the app Windows has for it.",
            ))),
            Kind::Archive => Some(Ok(Content::Elsewhere(
                "Archives open in the app Windows has for them.",
            ))),
            _ => None,
        };
        Preview {
            pane,
            name: name.to_string(),
            path: path.to_string(),
            place: place.to_string(),
            size,
            kind,
            content,
            live: false,
            asked_at: 0.0,
        }
    }

    /// Whether what it shows comes from reading the file.
    pub fn wants_data(&self) -> bool {
        self.content.is_none() || self.live
    }

    pub fn set_data(&mut self, ctx: &Context, data: Vec<u8>, size: u64) {
        self.size = Some(size);
        let truncated = size > data.len() as u64;
        self.content = Some(build(
            ctx, self.kind, &self.name, data, truncated, self.live,
        ));
    }
}

fn build(
    ctx: &Context,
    kind: Kind,
    name: &str,
    data: Vec<u8>,
    truncated: bool,
    tail: bool,
) -> Result<Content, String> {
    match kind {
        Kind::Image => {
            let image = image::load_from_memory(&data)
                .map_err(|e| format!("This picture can't be read: {e}"))?;
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
            Ok(Content::Image {
                texture: ctx.load_texture(name, pixels, TextureOptions::LINEAR),
                size,
                zoom: None,
            })
        }
        Kind::Svg => {
            let tree = resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default())
                .map_err(|e| format!("This drawing can't be read: {e}"))?;
            let natural = tree.size();
            let scale = (1600.0 / natural.width().max(natural.height())).min(8.0);
            let (w, h) = (
                (natural.width() * scale).ceil() as u32,
                (natural.height() * scale).ceil() as u32,
            );
            let mut pixmap =
                resvg::tiny_skia::Pixmap::new(w.max(1), h.max(1)).ok_or("This drawing is empty")?;
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            let rgba: Vec<u8> = pixmap
                .pixels()
                .iter()
                .flat_map(|p| {
                    let c = p.demultiply();
                    [c.red(), c.green(), c.blue(), c.alpha()]
                })
                .collect();
            let pixels = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            Ok(Content::Image {
                texture: ctx.load_texture(name, pixels, TextureOptions::LINEAR),
                size: [natural.width() as u32, natural.height() as u32],
                zoom: None,
            })
        }
        Kind::Audio => Audio::new(data).map(Content::Audio),
        Kind::Video | Kind::Document | Kind::Archive => Ok(Content::Elsewhere("")),
        Kind::Other => {
            let binary = data[..data.len().min(8192)].contains(&0);
            if binary {
                return Ok(Content::Hex {
                    dump: hex_dump(&data),
                });
            }
            let mut text = String::from_utf8_lossy(&data).into_owned();
            // The end of a log starts mid-line; that first part line is dropped.
            if tail && truncated {
                if let Some(at) = text.find('\n') {
                    text.drain(..=at);
                }
            }
            let lines: Vec<String> = text.lines().map(|l| l.replace('\t', "    ")).collect();
            let lang = highlight::lang_for(name);
            let mut state = State::default();
            let tokens = lines
                .iter()
                .map(|line| {
                    let mut out = Vec::new();
                    state = highlight::lex(lang, line, state, &mut out);
                    out
                })
                .collect();
            Ok(Content::Text {
                lines,
                tokens,
                truncated,
            })
        }
    }
}

fn hex_dump(data: &[u8]) -> String {
    let mut dump = String::new();
    for (row, chunk) in data[..data.len().min(HEX_BYTES)].chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|b| {
                if b.is_ascii_graphic() || *b == b' ' {
                    *b as char
                } else {
                    '·'
                }
            })
            .collect();
        dump.push_str(&format!(
            "{:08x}   {:<47}   {ascii}\n",
            row * 16,
            hex.join(" ")
        ));
    }
    dump
}

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn size_text(bytes: u64) -> String {
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KB", bytes as f64 / 1024.0),
        1_048_576..=1_073_741_823 => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
        _ => format!("{:.2} GB", bytes as f64 / 1_073_741_824.0),
    }
}

fn token_color(t: Token) -> Color32 {
    match t {
        Token::Text => theme::TEXT(),
        Token::Keyword => theme::KEYWORD(),
        Token::Type => theme::TYPE(),
        Token::Func => theme::FUNC(),
        Token::Str => theme::STRING(),
        Token::Number => theme::NUMBER(),
        Token::Comment => theme::COMMENT(),
        Token::Punct => theme::PUNCT(),
    }
}

fn icon_for(kind: Kind) -> char {
    match kind {
        Kind::Image | Kind::Svg => icons::IMAGE,
        Kind::Audio => icons::MUSIC,
        Kind::Other => icons::CODE,
        _ => icons::FILES,
    }
}

/// Shows the preview over the window; returns what was asked of it.
pub fn show(ctx: &Context, p: &mut Preview) -> Act {
    let mut act = Act::None;
    let screen = ctx.content_rect();
    let size = vec2(
        (screen.width() * 0.86).min(1500.0),
        (screen.height() * 0.86).min(1000.0),
    );
    let frame = Frame::NONE
        .fill(theme::PANEL_BG())
        .stroke(Stroke::new(1.0, theme::LINE()))
        .corner_radius(16)
        .shadow(egui::epaint::Shadow {
            offset: [0, 18],
            blur: 60,
            spread: 0,
            color: Color32::from_black_alpha(170),
        });
    let modal = egui::Modal::new(egui::Id::new("rdrop-preview"))
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha(160))
        .show(ctx, |ui| {
            ui.set_width(size.x);
            ui.set_height(size.y);
            ui.spacing_mut().item_spacing.y = 0.0;
            // The header: what it is, where, and what can be done with it.
            Frame::NONE
                .inner_margin(Margin {
                    left: 20,
                    right: 14,
                    top: 14,
                    bottom: 12,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let (disc, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                        ui.painter().circle_filled(
                            disc.center(),
                            18.0,
                            theme::ACCENT().gamma_multiply(0.16),
                        );
                        ui.painter().text(
                            disc.center(),
                            Align2::CENTER_CENTER,
                            icon_for(p.kind),
                            FontId::proportional(16.0),
                            theme::ACCENT(),
                        );
                        ui.add_space(10.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(
                                RichText::new(&p.name)
                                    .size(15.5)
                                    .strong()
                                    .color(theme::STRONG()),
                            );
                            let mut detail = format!("{}  ·  {}", p.place, p.path);
                            if let Some(s) = p.size {
                                detail = format!("{}  ·  {detail}", size_text(s));
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(detail).size(11.5).color(theme::COMMENT()),
                                )
                                .truncate(),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            if ui
                                .add(
                                    egui::Button::new(RichText::new(icons::CLOSE).size(14.0))
                                        .frame(false),
                                )
                                .on_hover_text("Close (Esc)")
                                .clicked()
                            {
                                act = Act::Close;
                            }
                            if ui::button(ui, "Open with app", ui::Kind::Quiet, true) {
                                act = Act::OpenElsewhere;
                            }
                            if p.kind == Kind::Other && is_log(&p.name) {
                                let mut live = p.live;
                                if ui::switch(ui, &mut live, "Live", "") {
                                    act = Act::Live(live);
                                }
                            }
                            if let Some(Ok(Content::Text { lines, .. })) = &p.content {
                                if ui::button(
                                    ui,
                                    &format!("{}  Copy", icons::COPY),
                                    ui::Kind::Quiet,
                                    true,
                                ) {
                                    ui.ctx().copy_text(lines.join("\n"));
                                }
                            }
                        });
                    });
                });
            let line = ui.available_rect_before_wrap();
            ui.painter()
                .hline(line.x_range(), line.top(), Stroke::new(1.0, theme::LINE()));
            // The body.
            let body = ui.available_rect_before_wrap();
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(body.shrink2(vec2(0.0, 1.0)))
                    .layout(Layout::top_down(Align::Min)),
            );
            body_ui(&mut child, p, &mut act);
        });
    let keys = ctx.input(|i| {
        (
            i.key_pressed(Key::Escape),
            i.key_pressed(Key::ArrowRight),
            i.key_pressed(Key::ArrowLeft),
            i.key_pressed(Key::Space),
        )
    });
    if keys.0 || modal.should_close() {
        act = Act::Close;
    }
    if act == Act::None && ctx.memory(|m| m.focused().is_none()) {
        if keys.1 {
            act = Act::Next;
        }
        if keys.2 {
            act = Act::Previous;
        }
        if keys.3 {
            if let Some(Ok(Content::Audio(a))) = &mut p.content {
                a.toggle();
            }
        }
    }
    act
}

fn body_ui(ui: &mut Ui, p: &mut Preview, act: &mut Act) {
    let centered = |ui: &mut Ui, f: &mut dyn FnMut(&mut Ui)| {
        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            ui.add_space((ui.available_height() * 0.3).max(20.0));
            f(ui);
        });
    };
    match &mut p.content {
        None => centered(ui, &mut |ui| {
            ui.spinner();
            ui.label(RichText::new("Reading…").color(theme::COMMENT()));
        }),
        Some(Err(e)) => {
            let e = e.clone();
            centered(ui, &mut |ui| {
                ui.label(RichText::new(ui::WARNING).size(36.0).color(ui::RED));
                ui.add_space(8.0);
                ui.label(RichText::new(&e).color(theme::TEXT()));
                ui.add_space(12.0);
                if ui::button(
                    ui,
                    "Open with app",
                    ui::Kind::Primary(theme::ACCENT()),
                    true,
                ) {
                    *act = Act::OpenElsewhere;
                }
            });
        }
        Some(Ok(Content::Elsewhere(reason))) => {
            let reason = *reason;
            centered(ui, &mut |ui| {
                ui.label(
                    RichText::new(icons::FILES)
                        .size(48.0)
                        .color(theme::ACCENT()),
                );
                ui.add_space(10.0);
                ui.label(RichText::new(reason).size(14.0).color(theme::TEXT()));
                ui.add_space(14.0);
                if ui::button(
                    ui,
                    "Open with app",
                    ui::Kind::Primary(theme::ACCENT()),
                    true,
                ) {
                    *act = Act::OpenElsewhere;
                }
            });
        }
        Some(Ok(Content::Image {
            texture,
            size,
            zoom,
        })) => {
            let real = vec2(size[0] as f32, size[1] as f32);
            // The wheel zooms around the pointer's place; a double click goes back to fit.
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 && ui.rect_contains_pointer(ui.max_rect()) {
                let avail = ui.available_size();
                let fit = (avail.x / real.x).min(avail.y / real.y).min(1.0);
                let z = zoom.unwrap_or(fit) * (1.0 + wheel * 0.0015);
                *zoom = Some(z.clamp(0.05, 16.0));
            }
            let avail = ui.available_size();
            let scale = zoom.unwrap_or_else(|| (avail.x / real.x).min(avail.y / real.y).min(1.0));
            let shown = real * scale;
            ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                let avail = ui.available_size();
                let pad = ((avail - shown) / 2.0).max(vec2(0.0, 0.0));
                ui.add_space(pad.y);
                ui.horizontal(|ui| {
                    ui.add_space(pad.x);
                    let resp = ui.add(
                        egui::Image::new(egui::load::SizedTexture::new(texture.id(), shown))
                            .sense(Sense::click()),
                    );
                    if resp.double_clicked() {
                        *zoom = None;
                    }
                });
            });
            let info = format!("{} × {}  ·  {:.0}%", size[0], size[1], scale * 100.0);
            let r = ui.max_rect();
            ui.painter().text(
                r.right_bottom() - vec2(14.0, 12.0),
                Align2::RIGHT_BOTTOM,
                info,
                FontId::proportional(11.5),
                theme::COMMENT(),
            );
        }
        Some(Ok(Content::Audio(audio))) => {
            if audio.playing() {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add_space((ui.available_height() * 0.22).max(10.0));
                let (disc, _) = ui.allocate_exact_size(vec2(120.0, 120.0), Sense::hover());
                let pulse = if audio.playing() {
                    (ui.input(|i| i.time) as f32 * 3.0).sin() * 4.0
                } else {
                    0.0
                };
                ui.painter().circle_filled(
                    disc.center(),
                    56.0 + pulse,
                    theme::ACCENT().gamma_multiply(0.12),
                );
                ui.painter().circle_filled(
                    disc.center(),
                    44.0,
                    theme::ACCENT().gamma_multiply(0.22),
                );
                ui.painter().text(
                    disc.center(),
                    Align2::CENTER_CENTER,
                    icons::MUSIC,
                    FontId::proportional(40.0),
                    theme::ACCENT(),
                );
                ui.add_space(14.0);
                ui.label(
                    RichText::new(&p.name)
                        .size(18.0)
                        .strong()
                        .color(theme::STRONG()),
                );
                ui.add_space(18.0);
                let playing = audio.playing();
                let icon = if playing { icons::PAUSE } else { icons::PLAY };
                let b = egui::Button::new(
                    RichText::new(icon)
                        .size(22.0)
                        .color(Color32::from_rgb(0x0b, 0x0b, 0x0f)),
                )
                .fill(theme::ACCENT())
                .corner_radius(28)
                .min_size(vec2(56.0, 56.0));
                if ui.add(b).on_hover_text("Play or pause (Space)").clicked() {
                    audio.toggle();
                }
                ui.add_space(16.0);
                let position = if audio.player.empty() {
                    Duration::ZERO
                } else {
                    audio.player.get_pos()
                };
                ui.allocate_ui_with_layout(
                    vec2(560.0, 30.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.label(
                            RichText::new(clock(position))
                                .monospace()
                                .size(12.0)
                                .color(theme::COMMENT()),
                        );
                        if let Some(total) = audio.duration.filter(|d| !d.is_zero()) {
                            let mut secs = position.as_secs_f32();
                            ui.spacing_mut().slider_width = ui.available_width() - 56.0;
                            if ui
                                .add(
                                    egui::Slider::new(&mut secs, 0.0..=total.as_secs_f32())
                                        .show_value(false),
                                )
                                .changed()
                            {
                                let _ = audio.player.try_seek(Duration::from_secs_f32(secs));
                            }
                            ui.label(
                                RichText::new(clock(total))
                                    .monospace()
                                    .size(12.0)
                                    .color(theme::COMMENT()),
                            );
                        }
                    },
                );
                ui.allocate_ui_with_layout(
                    vec2(220.0, 30.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.label(RichText::new(icons::VOLUME).color(theme::COMMENT()));
                        let mut volume = audio.player.volume();
                        ui.spacing_mut().slider_width = 160.0;
                        if ui
                            .add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false))
                            .changed()
                        {
                            audio.player.set_volume(volume);
                        }
                    },
                );
            });
        }
        Some(Ok(Content::Text {
            lines,
            tokens,
            truncated,
        })) => {
            let font = FontId::monospace(13.0);
            let row = ui.fonts_mut(|f| f.row_height(&font)) + 3.0;
            let gutter = (lines.len().max(1).to_string().len() as f32) * 8.0 + 22.0;
            if *truncated && !p.live {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.label(
                        RichText::new(format!(
                            "Showing the first {} of the file.",
                            size_text(TEXT_BYTES)
                        ))
                        .size(11.5)
                        .color(theme::COMMENT()),
                    );
                });
            }
            let mut area = ScrollArea::both().auto_shrink(false);
            if p.live {
                area = area.stick_to_bottom(true);
            }
            area.show_rows(ui, row, lines.len(), |ui, range| {
                for i in range {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let (num, _) = ui.allocate_exact_size(vec2(gutter, row), Sense::hover());
                        ui.painter().text(
                            num.right_center() - vec2(12.0, 0.0),
                            Align2::RIGHT_CENTER,
                            (i + 1).to_string(),
                            font.clone(),
                            theme::LINE_NUMBER(),
                        );
                        let mut job = LayoutJob::default();
                        let line = &lines[i];
                        let spans = &tokens[i];
                        let mut at = 0;
                        for (n, (start, kind)) in spans.iter().enumerate() {
                            if *start > at {
                                job.append(
                                    &line[at..*start],
                                    0.0,
                                    TextFormat::simple(font.clone(), theme::TEXT()),
                                );
                            }
                            let end = spans.get(n + 1).map_or(line.len(), |s| s.0);
                            if let Some(text) = line.get(*start..end) {
                                job.append(
                                    text,
                                    0.0,
                                    TextFormat::simple(font.clone(), token_color(*kind)),
                                );
                            }
                            at = end;
                        }
                        if at < line.len() || spans.is_empty() {
                            job.append(
                                line.get(at..).unwrap_or(line),
                                0.0,
                                TextFormat::simple(font.clone(), theme::TEXT()),
                            );
                        }
                        ui.add(egui::Label::new(job).extend());
                    });
                }
            });
        }
        Some(Ok(Content::Hex { dump })) => {
            ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Binary file: its first bytes, as hex.")
                                .size(11.5)
                                .color(theme::COMMENT()),
                        );
                        ui.add_space(6.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(dump.as_str())
                                    .monospace()
                                    .size(12.5)
                                    .color(theme::MUTED()),
                            )
                            .extend(),
                        );
                    });
                });
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_name() {
        assert_eq!(kind_of("photo.JPG"), Kind::Image);
        assert_eq!(kind_of("song.flac"), Kind::Audio);
        assert_eq!(kind_of("server.log"), Kind::Other);
        assert_eq!(kind_of("backup.tar.gz"), Kind::Archive);
        assert_eq!(kind_of("Makefile"), Kind::Other);
        assert!(is_log("nginx.log") && is_log("app.log.1") && !is_log("logo.png"));
        assert_eq!(read_limit(Kind::Video), 0);
    }
}
