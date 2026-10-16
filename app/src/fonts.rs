//! The fonts code is written in, for Settings → Appearance: the coding fonts most people
//! use, carried inside ride (with their licences in assets/fonts), and those Windows has.
//! The one picked draws the editor and the terminals.

use eframe::egui::{self, Context, FontData};

/// Where a font's file comes from.
pub enum Source {
    Bundled(&'static [u8]),
    /// A file in Windows' Fonts folder.
    System(&'static str),
}

pub struct CodeFont {
    pub name: &'static str,
    pub regular: Source,
    /// A separate bold file, for fonts that aren't variable.
    pub bold: Option<Source>,
}

macro_rules! bundled {
    ($file:literal) => {
        Source::Bundled(include_bytes!(concat!("../assets/fonts/", $file)))
    };
}

pub const DEFAULT: &str = "Cascadia Mono";

pub const FONTS: &[CodeFont] = &[
    CodeFont {
        name: "Cascadia Mono",
        regular: Source::System("CascadiaMono.ttf"),
        bold: None,
    },
    CodeFont {
        name: "Cascadia Code",
        regular: Source::System("CascadiaCode.ttf"),
        bold: None,
    },
    CodeFont {
        name: "JetBrains Mono",
        regular: bundled!("JetBrainsMono.ttf"),
        bold: None,
    },
    CodeFont {
        name: "Fira Code",
        regular: bundled!("FiraCode.ttf"),
        bold: None,
    },
    CodeFont {
        name: "Source Code Pro",
        regular: bundled!("SourceCodePro.ttf"),
        bold: None,
    },
    CodeFont {
        name: "Consolas",
        regular: Source::System("consola.ttf"),
        bold: Some(Source::System("consolab.ttf")),
    },
    CodeFont {
        name: "IBM Plex Mono",
        regular: bundled!("IBMPlexMono-Regular.ttf"),
        bold: Some(bundled!("IBMPlexMono-Bold.ttf")),
    },
    CodeFont {
        name: "Roboto Mono",
        regular: bundled!("RobotoMono.ttf"),
        bold: None,
    },
    CodeFont {
        name: "Ubuntu Mono",
        regular: bundled!("UbuntuMono-Regular.ttf"),
        bold: Some(bundled!("UbuntuMono-Bold.ttf")),
    },
    CodeFont {
        name: "Inconsolata",
        regular: bundled!("Inconsolata.ttf"),
        bold: None,
    },
    CodeFont {
        name: "Geist Mono",
        regular: bundled!("GeistMono.ttf"),
        bold: None,
    },
    CodeFont {
        name: "Victor Mono",
        regular: bundled!("VictorMono.ttf"),
        bold: None,
    },
    CodeFont {
        name: "Space Mono",
        regular: bundled!("SpaceMono-Regular.ttf"),
        bold: None,
    },
];

fn load(source: &Source) -> Option<FontData> {
    match source {
        Source::Bundled(bytes) => Some(FontData::from_static(bytes)),
        Source::System(file) => {
            let dir = std::path::PathBuf::from(
                std::env::var_os("WINDIR").unwrap_or("C:\\Windows".into()),
            )
            .join("Fonts");
            std::fs::read(dir.join(file)).ok().map(FontData::from_owned)
        }
    }
}

/// Whether the font can be used here (a Windows font may be missing).
pub fn available(font: &CodeFont) -> bool {
    match &font.regular {
        Source::Bundled(_) => true,
        Source::System(file) => {
            std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap_or("C:\\Windows".into()))
                .join("Fonts")
                .join(file)
                .exists()
        }
    }
}

/// Installs the fonts with `name` as the code font; every other one gets a family of its
/// own, so the picker can show each name in its own letters.
pub fn apply(ctx: &Context, name: &str) {
    let chosen = FONTS
        .iter()
        .find(|f| f.name == name && available(f))
        .or_else(|| FONTS.iter().find(|f| f.name == DEFAULT));
    let code = chosen.and_then(|f| load(&f.regular));
    let bold = chosen.and_then(|f| f.bold.as_ref()).and_then(load);
    let previews: Vec<(String, FontData)> = FONTS
        .iter()
        .filter(|f| matches!(f.regular, Source::Bundled(_)) || available(f))
        .filter_map(|f| Some((f.name.to_string(), load(&f.regular)?)))
        .collect();
    crate::theme::install_fonts_with(ctx, code, bold, previews);
}

/// A family that draws `name` in its own font, for previews.
pub fn family(name: &str) -> egui::FontFamily {
    egui::FontFamily::Name(name.into())
}
