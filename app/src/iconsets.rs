//! File icon themes from VS Code extensions (Material Icon Theme, vscode-icons and the like).
//! A theme maps file and folder names to image files; they are rasterised on first use.

use super::*;
use std::cell::RefCell;

/// Icons are drawn at 16 points; rasterising larger keeps them sharp on scaled displays.
const ICON_PX: u32 = 40;
const MANIFEST: &str = "ride-icon-theme.txt";

fn icon_sets_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("ride").join("icon-themes"))
}

fn file_safe(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_+()".contains(c) {
                c
            } else {
                ' '
            }
        })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.is_empty() {
        "Icons".into()
    } else {
        cleaned
    }
}

/// VS Code's language id for a file extension. Icon themes key many of their icons by
/// language rather than by extension.
fn language_of(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "json" => "json",
        "jsonc" => "jsonc",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "md" | "markdown" => "markdown",
        "lua" => "lua",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "cs" => "csharp",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "dart" => "dart",
        "php" => "php",
        "rb" => "ruby",
        "sh" | "bash" | "zsh" => "shellscript",
        "ps1" => "powershell",
        "bat" | "cmd" => "bat",
        "yml" | "yaml" => "yaml",
        "toml" => "toml",
        "xml" => "xml",
        "sql" => "sql",
        "vue" => "vue",
        "svelte" => "svelte",
        "ini" => "ini",
        "txt" => "plaintext",
        "log" => "log",
        "dockerfile" => "dockerfile",
        "makefile" => "makefile",
        _ => return None,
    })
}

/// Resolves `.` and `..` in a path inside an extension package.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn string_map(v: &serde_json::Value) -> HashMap<String, String> {
    v.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| Some((key.to_lowercase(), value.as_str()?.to_string())))
        .collect()
}

struct IconTheme {
    /// Icon id → image file.
    defs: HashMap<String, PathBuf>,
    file_names: HashMap<String, String>,
    file_exts: HashMap<String, String>,
    folder_names: HashMap<String, String>,
    folder_names_open: HashMap<String, String>,
    languages: HashMap<String, String>,
    file: Option<String>,
    folder: Option<String>,
    folder_open: Option<String>,
    /// Rasterised icons; `None` for one that couldn't be read, so it isn't retried.
    textures: RefCell<HashMap<String, Option<egui::TextureHandle>>>,
}

impl IconTheme {
    fn load(dir: &Path) -> Result<Self, String> {
        let manifest = std::fs::read_to_string(dir.join(MANIFEST)).map_err(|e| e.to_string())?;
        // Paths are kept as they were inside the package, so an icon's "../icons/x.svg"
        // resolves against the theme file's folder exactly as VS Code would.
        let json_relative = manifest.trim();
        let text = std::fs::read_to_string(dir.join(json_relative)).map_err(|e| e.to_string())?;
        let v = theme::parse_jsonc(&text)?;
        let base = json_relative
            .rsplit_once('/')
            .map_or("", |(folder, _)| folder);
        let defs = v["iconDefinitions"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(id, def)| {
                let relative = normalize(&format!("{base}/{}", def["iconPath"].as_str()?));
                Some((id.clone(), dir.join(relative)))
            })
            .collect();
        let name = |key: &str| v[key].as_str().map(str::to_string);
        Ok(Self {
            defs,
            file_names: string_map(&v["fileNames"]),
            file_exts: string_map(&v["fileExtensions"]),
            folder_names: string_map(&v["folderNames"]),
            folder_names_open: string_map(&v["folderNamesExpanded"]),
            languages: string_map(&v["languageIds"]),
            file: name("file"),
            folder: name("folder"),
            folder_open: name("folderExpanded"),
            textures: RefCell::default(),
        })
    }

    /// The icon id for an entry, the way VS Code picks it: by exact name, then by the
    /// longest matching extension, then by language, then the default.
    fn definition(&self, name: &str, is_dir: bool, open: bool) -> Option<&str> {
        let lower = name.to_lowercase();
        if is_dir {
            let closed = self.folder_names.get(&lower);
            let named = if open {
                self.folder_names_open.get(&lower).or(closed)
            } else {
                closed
            };
            let default = if open {
                self.folder_open.as_ref().or(self.folder.as_ref())
            } else {
                self.folder.as_ref()
            };
            return named.or(default).map(String::as_str);
        }
        if let Some(def) = self.file_names.get(&lower) {
            return Some(def);
        }
        // "types.d.ts" is tried as "d.ts", then as "ts".
        let mut rest = lower.as_str();
        while let Some((_, ext)) = rest.split_once('.') {
            if let Some(def) = self.file_exts.get(ext) {
                return Some(def);
            }
            rest = ext;
        }
        let ext = lower
            .rsplit_once('.')
            .map_or(lower.as_str(), |(_, ext)| ext);
        language_of(ext)
            .and_then(|language| self.languages.get(language))
            .or(self.file.as_ref())
            .map(String::as_str)
    }

    fn texture(&self, ctx: &Context, def: &str) -> Option<egui::TextureId> {
        let mut cache = self.textures.borrow_mut();
        if !cache.contains_key(def) {
            cache.insert(def.to_string(), self.rasterize(ctx, def));
        }
        cache[def].as_ref().map(|texture| texture.id())
    }

    fn rasterize(&self, ctx: &Context, def: &str) -> Option<egui::TextureHandle> {
        let path = self.defs.get(def)?;
        let data = std::fs::read(path).ok()?;
        let is_svg = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("svg"));
        let image = if is_svg {
            let tree =
                resvg::usvg::Tree::from_data(&data, &resvg::usvg::Options::default()).ok()?;
            let size = tree.size();
            let scale = ICON_PX as f32 / size.width().max(size.height());
            let mut pixmap = resvg::tiny_skia::Pixmap::new(ICON_PX, ICON_PX)?;
            // Centred, so an icon that isn't square keeps its proportions.
            let offset = |side: f32| (ICON_PX as f32 - side * scale) / 2.0;
            let transform = resvg::tiny_skia::Transform::from_scale(scale, scale)
                .post_translate(offset(size.width()), offset(size.height()));
            resvg::render(&tree, transform, &mut pixmap.as_mut());
            egui::ColorImage::from_rgba_premultiplied([ICON_PX as usize; 2], pixmap.data())
        } else {
            let rgba = image::load_from_memory(&data)
                .ok()?
                .thumbnail(ICON_PX, ICON_PX)
                .to_rgba8();
            egui::ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            )
        };
        Some(ctx.load_texture(
            format!("file-icon:{def}"),
            image,
            egui::TextureOptions::LINEAR,
        ))
    }
}

/// Unpacks the file icon themes of an extension package and returns their names.
/// An extension without any gives an empty list.
pub(super) fn install_icon_sets(bytes: &[u8]) -> Result<Vec<String>, String> {
    let root = icon_sets_dir().ok_or("no folder to keep icon themes in")?;
    unpack_icon_sets(bytes, &root)
}

/// Unpacks an extension's first file icon theme into a scratch folder of its own, for a
/// preview before installing. Returns the theme's folder, or `None` if it has none.
pub(super) fn unpack_for_preview(bytes: &[u8], id: &str) -> Result<Option<PathBuf>, String> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("TEMP"))
        .ok_or("no folder for previews")?;
    let root = PathBuf::from(base)
        .join("ride")
        .join("icon-preview")
        .join(file_safe(id));
    let _ = std::fs::remove_dir_all(&root);
    let names = unpack_icon_sets(bytes, &root)?;
    Ok(names.first().map(|name| root.join(name)))
}

fn unpack_icon_sets(bytes: &[u8], root: &Path) -> Result<Vec<String>, String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut read = |path: &str| -> Result<Vec<u8>, String> {
        let mut data = Vec::new();
        let mut file = archive.by_name(path).map_err(|e| format!("{path}: {e}"))?;
        file.read_to_end(&mut data)
            .map_err(|e| format!("{path}: {e}"))?;
        Ok(data)
    };
    let manifest = theme::parse_jsonc(&String::from_utf8_lossy(&read("extension/package.json")?))?;

    let mut installed = Vec::new();
    for entry in manifest["contributes"]["iconThemes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let Some(path) = entry["path"].as_str() else {
            continue;
        };
        let label = entry["label"]
            .as_str()
            .or(entry["id"].as_str())
            .unwrap_or("Icons");
        let json_path = normalize(&format!("extension/{path}"));
        let json = read(&json_path)?;
        let v = theme::parse_jsonc(&String::from_utf8_lossy(&json))?;
        let base = json_path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let icons: HashSet<String> = v["iconDefinitions"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(_, def)| {
                Some(normalize(&format!("{base}/{}", def["iconPath"].as_str()?)))
            })
            .collect();
        // Themes that draw their icons from a font have no image files; those aren't supported.
        if icons.is_empty() {
            continue;
        }

        let name = file_safe(label);
        let dir = root.join(&name);
        let _ = std::fs::remove_dir_all(&dir);
        let write = |relative: &str, data: &[u8]| -> Result<(), String> {
            let target = dir.join(relative);
            std::fs::create_dir_all(target.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
            std::fs::write(target, data).map_err(|e| e.to_string())
        };
        write(&json_path, &json)?;
        for icon in &icons {
            // A definition may point at a file the package doesn't ship; the rest still installs.
            if let Ok(data) = read(icon) {
                write(icon, &data)?;
            }
        }
        write(MANIFEST, json_path.as_bytes())?;
        installed.push(name);
    }
    Ok(installed)
}

#[derive(Default)]
pub(super) struct IconSets {
    /// The icon theme in use; empty for the built-in glyphs.
    pub(super) current: String,
    installed: Vec<String>,
    theme: Option<IconTheme>,
    /// While a Marketplace icon theme is previewed: its id, and the theme it stands in for.
    preview: Option<(String, Option<IconTheme>)>,
}

impl App {
    pub(super) fn refresh_icon_sets(&mut self) {
        let mut names: Vec<String> = icon_sets_dir()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().join(MANIFEST).is_file())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        self.icon_sets.installed = names;
    }

    /// Switches the file icons; an empty name goes back to the built-in ones.
    pub(super) fn select_icon_set(&mut self, name: &str) {
        // A choice ends any preview; it mustn't be undone when the preview ends.
        self.icon_sets.preview = None;
        let loaded = match (name.is_empty(), icon_sets_dir()) {
            (true, _) | (_, None) => Ok(None),
            (false, Some(dir)) => IconTheme::load(&dir.join(name)).map(Some),
        };
        match loaded {
            Ok(theme) => {
                self.icon_sets.theme = theme;
                self.icon_sets.current = name.to_string();
            }
            Err(error) => self.status = format!("Couldn't load the {name} icons: {error}"),
        }
    }

    /// Shows the file icons unpacked in `dir` until `end_icon_preview`, without choosing them.
    pub(super) fn preview_icon_set(&mut self, id: &str, dir: &Path) {
        if self
            .icon_sets
            .preview
            .as_ref()
            .is_some_and(|(shown, _)| shown == id)
        {
            return;
        }
        let Ok(theme) = IconTheme::load(dir) else {
            return;
        };
        let replaced = std::mem::replace(&mut self.icon_sets.theme, Some(theme));
        let chosen = match self.icon_sets.preview.take() {
            Some((_, chosen)) => chosen,
            None => replaced,
        };
        self.icon_sets.preview = Some((id.to_string(), chosen));
    }

    /// Puts the chosen file icons back after a preview.
    pub(super) fn end_icon_preview(&mut self) {
        if let Some((_, chosen)) = self.icon_sets.preview.take() {
            self.icon_sets.theme = chosen;
        }
    }

    /// The icon image for a tree entry, when an icon theme is in use and has one for it.
    pub(super) fn entry_icon(
        &self,
        ctx: &Context,
        name: &str,
        is_dir: bool,
        open: bool,
    ) -> Option<egui::TextureId> {
        let theme = self.icon_sets.theme.as_ref()?;
        theme.texture(ctx, theme.definition(name, is_dir, open)?)
    }

    /// The "File icons" part of the installed list: the built-in set, then the installed ones.
    pub(super) fn icon_set_rows(
        &self,
        ui: &mut Ui,
        select: &mut Option<String>,
        remove: &mut Option<String>,
    ) {
        ui.add_space(12.0);
        ui.label(
            RichText::new("FILE ICONS")
                .small()
                .strong()
                .color(theme::COMMENT()),
        );
        ui.add_space(4.0);
        let names = std::iter::once(String::new()).chain(self.icon_sets.installed.iter().cloned());
        for name in names {
            let current = name == self.icon_sets.current;
            let label = if name.is_empty() {
                "Built-in"
            } else {
                name.as_str()
            };
            let mut on_button = false;
            let row = Frame::NONE
                .fill(if current {
                    theme::tint(18)
                } else {
                    Color32::TRANSPARENT
                })
                .corner_radius(8)
                .inner_margin(Margin::symmetric(9, 7))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if !name.is_empty()
                                && icon_button(ui, icons::TRASH, "Remove these icons")
                            {
                                *remove = Some(name.clone());
                                on_button = true;
                            }
                            if current {
                                ui.label(RichText::new(icons::CHECK).color(theme::STRING()));
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(egui::Label::new(label).selectable(false));
                            });
                        });
                    });
                })
                .response;
            if row.contains_pointer() {
                ui.painter().rect_stroke(
                    row.rect,
                    8,
                    (1.0, theme::tint(46)),
                    egui::StrokeKind::Inside,
                );
                if !on_button && ui.input(|i| i.pointer.primary_clicked()) {
                    *select = Some(name.clone());
                }
            }
        }
    }

    pub(super) fn remove_icon_set(&mut self, name: &str) {
        if let Some(dir) = icon_sets_dir() {
            let _ = std::fs::remove_dir_all(dir.join(name));
        }
        self.refresh_icon_sets();
        if self.icon_sets.current == name {
            self.select_icon_set("");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_paths_are_normalised() {
        assert_eq!(
            normalize("extension/./dist/../icons/git.svg"),
            "extension/icons/git.svg"
        );
        assert_eq!(
            normalize("extension/dist/material-icons.json"),
            "extension/dist/material-icons.json"
        );
    }

    #[test]
    fn icons_are_picked_by_name_then_extension_then_language() {
        let pairs = |list: &[(&str, &str)]| {
            list.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let theme = IconTheme {
            defs: HashMap::new(),
            file_names: pairs(&[("package.json", "npm")]),
            file_exts: pairs(&[("d.ts", "typings"), ("lock", "lock")]),
            folder_names: pairs(&[("src", "folder-src")]),
            folder_names_open: pairs(&[("src", "folder-src-open")]),
            languages: pairs(&[("rust", "rust"), ("typescript", "ts")]),
            file: Some("file".into()),
            folder: Some("folder".into()),
            folder_open: Some("folder-open".into()),
            textures: RefCell::default(),
        };
        assert_eq!(theme.definition("Package.json", false, false), Some("npm"));
        assert_eq!(
            theme.definition("types.d.ts", false, false),
            Some("typings")
        );
        assert_eq!(theme.definition("index.ts", false, false), Some("ts"));
        assert_eq!(theme.definition("main.rs", false, false), Some("rust"));
        assert_eq!(
            theme.definition("notes.unknown", false, false),
            Some("file")
        );
        assert_eq!(theme.definition("src", true, true), Some("folder-src-open"));
        assert_eq!(theme.definition("src", true, false), Some("folder-src"));
        assert_eq!(theme.definition("other", true, true), Some("folder-open"));
    }
}

#[cfg(test)]
mod package_tests {
    use super::*;

    /// Installs a real icon extension into a scratch folder and draws one of its icons, e.g.
    /// `RIDE_SAMPLE_ICON_VSIX=material-icons.vsix cargo test`.
    #[test]
    fn a_real_icon_package_installs_and_renders() {
        let Some(path) = std::env::var_os("RIDE_SAMPLE_ICON_VSIX") else {
            return;
        };
        let scratch = std::env::temp_dir().join(format!("ride-icon-test-{}", std::process::id()));
        // Only this test reads APPDATA, so redirecting it doesn't disturb the others.
        std::env::set_var("APPDATA", &scratch);

        let names = install_icon_sets(&std::fs::read(path).unwrap()).unwrap();
        assert!(
            !names.is_empty(),
            "the package should contain an icon theme"
        );
        let theme =
            IconTheme::load(&scratch.join("ride").join("icon-themes").join(&names[0])).unwrap();
        println!("{}: {} icons", names[0], theme.defs.len());

        let ctx = Context::default();
        for (name, is_dir) in [
            ("main.rs", false),
            ("package.json", false),
            ("src", true),
            ("notes.xyz", false),
        ] {
            let def = theme
                .definition(name, is_dir, false)
                .unwrap_or_else(|| panic!("no icon for {name}"));
            assert!(
                theme.defs[def].is_file(),
                "{name}: {def} should have been unpacked"
            );
            assert!(
                theme.texture(&ctx, def).is_some(),
                "{name}: {def} should rasterise"
            );
            println!("  {name} -> {def}");
        }
        let _ = std::fs::remove_dir_all(scratch);
    }
}
