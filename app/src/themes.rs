//! Colour themes: the built-in ones, VS Code theme files, and a browser for the
//! themes on the VS Code Marketplace.

use super::*;

const BUILT_IN: [(&str, theme::Palette); 3] = [
    ("Graphite", theme::GRAPHITE),
    ("Slate", theme::SLATE),
    ("Day", theme::DAY),
];

const QUERY_URL: &str = "https://marketplace.visualstudio.com/_apis/public/gallery/extensionquery";
/// The Marketplace's own sort codes.
const SORTS: [(&str, u32); 3] = [("Most installed", 4), ("Top rated", 12), ("Trending", 8)];

/// A theme extension as listed by the Marketplace.
struct Extension {
    /// `publisher.name`, which is also what installs it.
    id: String,
    name: String,
    publisher: String,
    description: String,
    installs: f64,
    rating: f64,
    icon_url: Option<String>,
}

/// What an extension package added: colour theme names, then icon theme names.
type Installed = (Vec<String>, Vec<String>);
type SearchResult = Result<(Vec<Extension>, u64), String>;
/// A downloaded icon: the search it belongs to, the extension id, its size and RGBA pixels.
type Icon = (u64, String, [usize; 2], Vec<u8>);

pub(super) struct Themes {
    pub(super) current: String,
    /// The theme on screen, which differs from `current` during a preview.
    shown: String,
    /// The main window is see-through over a blurred backdrop.
    pub(super) glass: bool,
    /// How opaque the panes are while glass is on, 0 to 1.
    pub(super) opacity: f32,
    /// The glass state and tint last given to the window, so it is set only on change.
    applied: Option<(bool, bool)>,
    /// The window's system frame has been taken over (done once, on the first frame).
    adopted: bool,
    /// Every theme that can be applied, with its colours for the swatches.
    installed: Vec<(String, theme::Palette, bool)>,
    query: String,
    sort: u32,
    results: Vec<Extension>,
    /// How many themes the Marketplace has for the current search.
    total: u64,
    searching: Option<Receiver<SearchResult>>,
    searched: bool,
    error: String,
    /// Bumped per search, so icons arriving for an older one are dropped.
    generation: u64,
    icons: HashMap<String, egui::TextureHandle>,
    icon_tx: Sender<Icon>,
    icon_rx: Receiver<Icon>,
    /// The extension being installed; yields the names of the themes it added.
    installing: Option<(String, Receiver<Result<Installed, String>>)>,
    /// The Marketplace list shows file icon themes instead of colour themes.
    icons_mode: bool,
    /// Extensions installed during this run, for the "Installed" mark on their cards.
    added: HashSet<String>,
    message: String,
    /// Packages fetched for previews in the command palette, by extension id.
    packs: HashMap<String, PackState>,
}

impl Default for Themes {
    fn default() -> Self {
        let (icon_tx, icon_rx) = channel();
        Self {
            current: "Graphite".into(),
            shown: "Graphite".into(),
            glass: false,
            opacity: 0.78,
            applied: None,
            adopted: false,
            installed: Vec::new(),
            query: String::new(),
            sort: SORTS[0].1,
            results: Vec::new(),
            total: 0,
            searching: None,
            searched: false,
            error: String::new(),
            generation: 0,
            icons: HashMap::new(),
            icon_tx,
            icon_rx,
            installing: None,
            icons_mode: false,
            added: HashSet::new(),
            message: String::new(),
            packs: HashMap::new(),
        }
    }
}

fn themes_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(base).join("ride").join("themes"))
}

/// A theme's label, reduced to what is safe as a file name.
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
        "Theme".into()
    } else {
        cleaned
    }
}

/// Stores a VS Code theme (its JSON text) under `name` and returns the name it got.
fn save_theme(name: &str, text: &str) -> Result<String, String> {
    theme::parse_jsonc(text).map_err(|e| format!("{name} isn't a JSON colour theme: {e}"))?;
    let dir = themes_dir().ok_or("no folder to keep themes in")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = file_safe(name);
    std::fs::write(dir.join(format!("{name}.json")), text).map_err(|e| e.to_string())?;
    Ok(name)
}

/// The colour themes inside a `.vsix` extension package, as `(label, JSON text)`.
fn themes_in_vsix(bytes: Vec<u8>) -> Result<Vec<(String, String)>, String> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let mut read = |path: &str| -> Result<String, String> {
        let mut text = String::new();
        let mut file = archive.by_name(path).map_err(|e| format!("{path}: {e}"))?;
        file.read_to_string(&mut text)
            .map_err(|e| format!("{path}: {e}"))?;
        Ok(text)
    };
    let manifest = theme::parse_jsonc(&read("extension/package.json")?)?;
    let mut found = Vec::new();
    for entry in manifest["contributes"]["themes"]
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
            .unwrap_or("Theme");
        // Older themes use the TextMate .tmTheme format, which isn't read here.
        if !path.to_lowercase().ends_with(".json") {
            continue;
        }
        let path = format!("extension/{}", path.trim_start_matches("./"));
        found.push((label.to_string(), read(&path)?));
    }
    if found.is_empty() {
        return Err("this extension has no JSON colour themes (it may be an icon theme)".into());
    }
    Ok(found)
}

/// `publisher.extension`, typed as is or taken from a Marketplace link.
fn marketplace_id(input: &str) -> Option<(String, String)> {
    let input = input.trim();
    let id = input.split("itemName=").nth(1).unwrap_or(input);
    let id = id.split(['&', '#', ' ']).next()?;
    let (publisher, name) = id.split_once('.')?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    (ok(publisher) && ok(name)).then(|| (publisher.to_string(), name.to_string()))
}

fn download(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut response = ureq::get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| format!("download failed: {e}"))
}

/// Installs everything usable in an extension package: its colour themes and its file icons.
fn install_package(bytes: Vec<u8>) -> Result<Installed, String> {
    let icon_sets = iconsets::install_icon_sets(&bytes)?;
    let themes = match themes_in_vsix(bytes) {
        Ok(themes) => themes
            .iter()
            .map(|(label, text)| save_theme(label, text))
            .collect::<Result<Vec<_>, _>>()?,
        // An icon pack has no colour themes, and that is fine.
        Err(_) if !icon_sets.is_empty() => Vec::new(),
        Err(_) => {
            return Err(
                "this extension has no colour themes or file icons that ride can use".into(),
            )
        }
    };
    Ok((themes, icon_sets))
}

fn install(publisher: &str, name: &str) -> Result<Installed, String> {
    let url = format!(
        "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/{publisher}/vsextensions/{name}/latest/vspackage"
    );
    install_package(download(&url, 120 * 1024 * 1024)?)
}

/// Asks the Marketplace for theme extensions matching `query`.
fn search(query: &str, sort: u32, icons: bool) -> SearchResult {
    use serde_json::json;
    // Extensions are tagged by what they contribute.
    let wanted = if icons { "icon-theme" } else { "color-theme" };
    let mut criteria = vec![
        json!({ "filterType": 8, "value": "Microsoft.VisualStudio.Code" }),
        json!({ "filterType": 5, "value": "Themes" }),
    ];
    if icons {
        criteria.push(json!({ "filterType": 1, "value": wanted }));
    }
    if !query.trim().is_empty() {
        criteria.push(json!({ "filterType": 10, "value": query.trim() }));
    }
    let body = json!({
        "filters": [{ "criteria": criteria, "pageNumber": 1, "pageSize": 60, "sortBy": sort, "sortOrder": 0 }],
        // Latest version only, with statistics, tags and file links.
        "flags": 918,
    })
    .to_string();
    let mut response = ureq::post(QUERY_URL)
        .header("Accept", "application/json;api-version=3.0-preview.1")
        .header("Content-Type", "application/json")
        .send(body.as_str())
        .map_err(|e| format!("The Marketplace can't be reached: {e}"))?;
    let text = response
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let result = &value["results"][0];
    let total = result["resultMetadata"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["metadataType"] == "ResultCount")
        .find_map(|m| m["metadataItems"][0]["count"].as_u64())
        .unwrap_or(0);

    let extensions = result["extensions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| {
            // The Themes category also holds icon packs; those have nothing to install here.
            let tags: Vec<&str> = e["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t.as_str())
                .collect();
            tags.is_empty() || tags.contains(&wanted)
        })
        .filter_map(|e| {
            let publisher = e["publisher"]["publisherName"].as_str()?;
            let name = e["extensionName"].as_str()?;
            let stat = |wanted: &str| {
                e["statistics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|s| s["statisticName"] == wanted)
                    .and_then(|s| s["value"].as_f64())
                    .unwrap_or(0.0)
            };
            let icon_url = e["versions"][0]["files"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|f| f["assetType"] == "Microsoft.VisualStudio.Services.Icons.Default")
                .and_then(|f| f["source"].as_str())
                .map(str::to_string);
            Some(Extension {
                id: format!("{publisher}.{name}"),
                name: e["displayName"].as_str().unwrap_or(name).to_string(),
                publisher: e["publisher"]["displayName"]
                    .as_str()
                    .unwrap_or(publisher)
                    .to_string(),
                description: e["shortDescription"].as_str().unwrap_or("").to_string(),
                installs: stat("install"),
                rating: stat("averagerating"),
                icon_url,
            })
        })
        .collect();
    Ok((extensions, total))
}

/// A Marketplace result as the command palette lists it.
pub(super) struct MarketHit {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) publisher: String,
    pub(super) installs: f64,
    /// A file icon theme rather than a colour theme.
    pub(super) icons: bool,
}

/// The palette's Marketplace search: the most installed colour themes and file icon
/// themes matching `query`, a few of each.
pub(super) fn palette_search(query: &str) -> Result<Vec<MarketHit>, String> {
    const EACH: usize = 6;
    let mut hits = Vec::new();
    for icons in [false, true] {
        let (extensions, _) = search(query, SORTS[0].1, icons)?;
        hits.extend(extensions.into_iter().take(EACH).map(|e| MarketHit {
            id: e.id,
            name: e.name,
            publisher: e.publisher,
            installs: e.installs,
            icons,
        }));
    }
    Ok(hits)
}

/// A Marketplace package fetched for a preview: kept, so installing it needs no second download.
pub(super) struct Pack {
    bytes: Vec<u8>,
    /// Its colour themes, ready to show.
    palettes: Vec<theme::Palette>,
    /// Its first file icon theme, unpacked in a scratch folder.
    icon_dir: Option<PathBuf>,
}

pub(super) enum PackState {
    Loading(Receiver<Result<Pack, String>>),
    Ready(Pack),
    Failed,
}

fn fetch_pack(id: &str) -> Result<Pack, String> {
    let (publisher, name) = marketplace_id(id).ok_or("not a Marketplace id")?;
    let url = format!(
        "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/{publisher}/vsextensions/{name}/latest/vspackage"
    );
    let bytes = download(&url, 120 * 1024 * 1024)?;
    let palettes = themes_in_vsix(bytes.clone())
        .unwrap_or_default()
        .iter()
        .filter_map(|(_, text)| theme::parse_jsonc(text).ok())
        .map(|value| theme::from_vscode(&value))
        .collect();
    let icon_dir = iconsets::unpack_for_preview(&bytes, id).ok().flatten();
    Ok(Pack {
        bytes,
        palettes,
        icon_dir,
    })
}

/// Downloads the cards' icons in the background, a few at a time.
fn fetch_icons(generation: u64, wanted: Vec<(String, String)>, tx: Sender<Icon>, ctx: Context) {
    const WORKERS: usize = 6;
    let queue = Arc::new(Mutex::new(wanted));
    for _ in 0..WORKERS {
        let (queue, tx, ctx) = (queue.clone(), tx.clone(), ctx.clone());
        thread::spawn(move || loop {
            let Some((id, url)) = queue.lock().unwrap().pop() else {
                break;
            };
            let Ok(bytes) = download(&url, 4 * 1024 * 1024) else {
                continue;
            };
            let Ok(image) = image::load_from_memory(&bytes) else {
                continue;
            };
            let small = image.thumbnail(96, 96).to_rgba8();
            let size = [small.width() as usize, small.height() as usize];
            if tx.send((generation, id, size, small.into_raw())).is_err() {
                break;
            }
            ctx.request_repaint();
        });
    }
}

pub(super) fn short_count(n: f64) -> String {
    match n {
        n if n >= 1e6 => format!("{:.1}M", n / 1e6),
        n if n >= 1e3 => format!("{:.0}K", n / 1e3),
        n => format!("{n:.0}"),
    }
}

impl App {
    pub(super) fn refresh_themes(&mut self) {
        let mut installed: Vec<(String, theme::Palette, bool)> = BUILT_IN
            .iter()
            .map(|(name, palette)| (name.to_string(), *palette, true))
            .collect();
        let mut imported: Vec<(String, theme::Palette, bool)> = themes_dir()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension()? != "json" {
                    return None;
                }
                let value = theme::parse_jsonc(&std::fs::read_to_string(&path).ok()?).ok()?;
                let name = path.file_stem()?.to_string_lossy().into_owned();
                Some((name, theme::from_vscode(&value), false))
            })
            .collect();
        imported.sort_by_key(|(name, _, _)| name.to_lowercase());
        installed.extend(imported);
        self.themes.installed = installed;
    }

    pub(super) fn theme_names(&self) -> Vec<String> {
        self.themes
            .installed
            .iter()
            .map(|(name, _, _)| name.clone())
            .collect()
    }

    pub(super) fn select_theme(&mut self, ctx: &Context, name: &str) {
        match self
            .themes
            .installed
            .iter()
            .find(|(installed, _, _)| installed == name)
        {
            Some((_, palette, _)) => {
                theme::set(ctx, *palette);
                self.themes.current = name.to_string();
                self.themes.shown = name.to_string();
            }
            None => self.themes.message = format!("{name} isn't installed any more"),
        }
    }

    /// Shows a theme without choosing it. `None` goes back to the chosen one.
    pub(super) fn preview_theme(&mut self, ctx: &Context, name: Option<&str>) {
        let name = name.unwrap_or(&self.themes.current).to_string();
        if self.themes.shown == name {
            return;
        }
        let found = self
            .themes
            .installed
            .iter()
            .find(|(installed, _, _)| *installed == name);
        if let Some((_, palette, _)) = found {
            theme::set(ctx, *palette);
            self.themes.shown = name;
        }
    }

    /// Gives the main window its blurred backdrop, or takes it away, to match the setting.
    pub(super) fn sync_glass(&mut self, frame: &eframe::Frame) {
        if !std::mem::replace(&mut self.themes.adopted, true) {
            os::adopt_window(frame);
        }
        theme::set_glass(self.themes.glass.then_some(self.themes.opacity));
        // The backdrop's tint follows whether the theme is dark or light.
        let dark = theme::STRONG() == Color32::WHITE;
        let wanted = (self.themes.glass, dark);
        if self.themes.applied == Some(wanted) {
            return;
        }
        self.themes.applied = Some(wanted);
        if !os::set_backdrop(frame, self.themes.glass, dark) && self.themes.glass {
            self.themes.glass = false;
            theme::set_glass(None);
            self.themes.message = "Glass isn't available on this version of Windows".into();
        }
    }

    fn import_theme_file(&mut self, ctx: &Context) {
        let picked = rfd::FileDialog::new()
            .set_title("Import a VS Code theme")
            .add_filter("VS Code theme or extension", &["json", "jsonc", "vsix"])
            .pick_file();
        let Some(path) = picked else { return };
        let is_vsix = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("vsix"));
        let added: Result<Installed, String> = if is_vsix {
            std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(install_package)
        } else {
            std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| {
                    let stem = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let name = theme::parse_jsonc(&text)
                        .ok()
                        .and_then(|v| v["name"].as_str().map(str::to_string));
                    save_theme(&name.unwrap_or(stem), &text).map(|name| (vec![name], Vec::new()))
                })
        };
        self.themes_added(ctx, added);
    }

    /// Takes in what an install or import produced, and switches to the first new thing.
    fn themes_added(&mut self, ctx: &Context, added: Result<Installed, String>) {
        match added {
            Ok((themes, icon_sets)) => {
                self.refresh_themes();
                self.refresh_icon_sets();
                let all: Vec<&str> = themes
                    .iter()
                    .chain(&icon_sets)
                    .map(String::as_str)
                    .collect();
                self.themes.message = format!("Added {}", all.join(", "));
                if let Some(first) = themes.first() {
                    self.select_theme(ctx, first);
                }
                if let Some(first) = icon_sets.first() {
                    self.select_icon_set(first);
                }
            }
            Err(error) => self.themes.message = error,
        }
    }

    /// Previews a Marketplace extension from the palette: its first colour theme on the
    /// whole window, or its file icons in the tree. The package is fetched first; the
    /// answer says how far that is, for the palette row.
    pub(super) fn market_preview(&mut self, ctx: &Context, id: &str, icons: bool) -> String {
        let arrived = match self.themes.packs.get(id) {
            Some(PackState::Loading(rx)) => rx.try_recv().ok(),
            _ => None,
        };
        if let Some(result) = arrived {
            let state = match result {
                Ok(pack) => PackState::Ready(pack),
                Err(_) => PackState::Failed,
            };
            self.themes.packs.insert(id.to_string(), state);
        }
        let ready = match self.themes.packs.get(id) {
            None => {
                // Each package is several megabytes; only the most recent few are kept.
                if self.themes.packs.len() >= 10 {
                    self.themes
                        .packs
                        .retain(|_, state| matches!(state, PackState::Loading(_)));
                }
                let (tx, rx) = channel();
                let (owned, ctx) = (id.to_string(), ctx.clone());
                thread::spawn(move || {
                    let _ = tx.send(fetch_pack(&owned));
                    ctx.request_repaint();
                });
                self.themes
                    .packs
                    .insert(id.to_string(), PackState::Loading(rx));
                return "Loading previewâ€¦".into();
            }
            Some(PackState::Loading(_)) => return "Loading previewâ€¦".into(),
            Some(PackState::Failed) => return "No preview".into(),
            Some(PackState::Ready(pack)) => {
                if icons {
                    pack.icon_dir.clone().map(Err)
                } else {
                    pack.palettes.first().copied().map(Ok)
                }
            }
        };
        match ready {
            Some(Ok(palette)) => {
                let key = format!("marketplace:{id}");
                if self.themes.shown != key {
                    theme::set(ctx, palette);
                    self.themes.shown = key;
                }
                "Previewing".into()
            }
            Some(Err(dir)) => {
                self.preview_icon_set(id, &dir);
                "Previewing in the file tree".into()
            }
            None => "No preview".into(),
        }
    }

    /// Installs a Marketplace extension, from the package a preview already fetched when
    /// there is one.
    pub(super) fn install_market(&mut self, ctx: &Context, id: &str) {
        if self.themes.installing.is_some() {
            return;
        }
        let Some(PackState::Ready(pack)) = self.themes.packs.get(id) else {
            return self.install_extension(ctx, id);
        };
        let (tx, rx) = channel();
        let (bytes, ctx) = (pack.bytes.clone(), ctx.clone());
        thread::spawn(move || {
            let _ = tx.send(install_package(bytes));
            ctx.request_repaint();
        });
        self.themes.installing = Some((id.to_string(), rx));
        self.themes.message.clear();
    }

    /// Whether an extension was installed during this run, and which one is being installed.
    pub(super) fn extension_state(&self, id: &str) -> (bool, bool) {
        let installing = self
            .themes
            .installing
            .as_ref()
            .is_some_and(|(busy, _)| busy == id);
        (self.themes.added.contains(id), installing)
    }

    pub(super) fn install_extension(&mut self, ctx: &Context, id: &str) {
        // One install at a time keeps the result unambiguous.
        if self.themes.installing.is_some() {
            return;
        }
        let Some((publisher, name)) = marketplace_id(id) else {
            return;
        };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(install(&publisher, &name));
            ctx.request_repaint();
        });
        self.themes.installing = Some((id.to_string(), rx));
        self.themes.message.clear();
    }

    fn start_search(&mut self, ctx: &Context) {
        let (tx, rx) = channel();
        let (query, sort, ctx) = (self.themes.query.clone(), self.themes.sort, ctx.clone());
        let icons = self.themes.icons_mode;
        thread::spawn(move || {
            let _ = tx.send(search(&query, sort, icons));
            ctx.request_repaint();
        });
        self.themes.searching = Some(rx);
        self.themes.searched = true;
        self.themes.error.clear();
    }

    /// Collects what the background threads finished: searches, icons, installs.
    /// Runs every frame, as an install started from the palette finishes outside this view.
    pub(super) fn poll_themes(&mut self, ctx: &Context) {
        if let Some(result) = self
            .themes
            .searching
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.themes.searching = None;
            match result {
                Ok((extensions, total)) => {
                    self.themes.generation += 1;
                    let missing: Vec<(String, String)> = extensions
                        .iter()
                        .filter(|e| !self.themes.icons.contains_key(&e.id))
                        .filter_map(|e| Some((e.id.clone(), e.icon_url.clone()?)))
                        .rev()
                        .collect();
                    fetch_icons(
                        self.themes.generation,
                        missing,
                        self.themes.icon_tx.clone(),
                        ctx.clone(),
                    );
                    self.themes.results = extensions;
                    self.themes.total = total;
                }
                Err(error) => self.themes.error = error,
            }
        }
        while let Ok((generation, id, size, pixels)) = self.themes.icon_rx.try_recv() {
            if generation == self.themes.generation {
                let image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
                let texture =
                    ctx.load_texture(format!("icon:{id}"), image, egui::TextureOptions::LINEAR);
                self.themes.icons.insert(id, texture);
            }
        }
        let finished = self
            .themes
            .installing
            .as_ref()
            .and_then(|(id, rx)| rx.try_recv().ok().map(|result| (id.clone(), result)));
        if let Some((id, result)) = finished {
            self.themes.installing = None;
            if result.is_ok() {
                self.themes.added.insert(id);
            }
            self.themes_added(ctx, result);
            self.status = self.themes.message.clone();
        }
    }

    /// The whole Themes view: what is installed on the left, the Marketplace on the right.
    pub(super) fn themes_view(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.poll_themes(&ctx);
        if !self.themes.searched {
            self.start_search(&ctx);
        }
        // On a narrow window the Marketplace alone is shown; the list of installed themes needs room.
        let width = ui.available_width();
        if width >= 620.0 {
            Panel::left("installed_themes")
                .resizable(false)
                .exact_size(264.0_f32.min(width * 0.36))
                .show_separator_line(true)
                .frame(theme::card(theme::glass()).inner_margin(Margin::same(14)))
                .show(ui, |ui| self.installed_themes(ui));
        }
        CentralPanel::default_margins()
            .frame(Frame::NONE.fill(theme::surface()).inner_margin(Margin {
                left: 30,
                right: 14,
                top: 22,
                bottom: 0,
            }))
            .show(ui, |ui| self.marketplace(ui));
    }

    fn installed_themes(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.label(
            RichText::new("INSTALLED")
                .small()
                .strong()
                .color(theme::COMMENT()),
        );
        ui.add_space(4.0);
        let (mut select, mut remove) = (None, None);
        let (mut select_icons, mut remove_icons) = (None, None);
        egui::Panel::bottom("import_theme")
            .frame(Frame::NONE.inner_margin(Margin {
                left: 0,
                right: 0,
                top: 10,
                bottom: 0,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.label(
                    RichText::new("WINDOW")
                        .small()
                        .strong()
                        .color(theme::COMMENT()),
                );
                ui.checkbox(&mut self.themes.glass, "Glass").on_hover_text(
                    "Makes the window see-through over a blurred view of what is behind it",
                );
                if self.themes.glass {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Opacity").small().color(theme::COMMENT()));
                        ui.spacing_mut().slider_width = 120.0;
                        ui.add(
                            egui::Slider::new(&mut self.themes.opacity, 0.30..=0.97)
                                .show_value(false),
                        );
                        let percent = format!("{:.0}%", self.themes.opacity * 100.0);
                        ui.label(RichText::new(percent).small().color(theme::COMMENT()));
                    });
                }
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(4.0);
                if ui
                    .button(format!("{}  Import a theme fileâ€¦", icons::ADD))
                    .clicked()
                {
                    self.import_theme_file(&ctx);
                }
                ui.label(
                    RichText::new(".json or .vsix")
                        .small()
                        .color(theme::COMMENT()),
                );
            });
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            for (name, palette, built_in) in &self.themes.installed {
                let current = *name == self.themes.current;
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
                            // The theme at a glance: its background with three of its syntax colours.
                            let (swatch, _) =
                                ui.allocate_exact_size(vec2(40.0, 22.0), Sense::hover());
                            let painter = ui.painter();
                            painter.rect(
                                swatch,
                                5,
                                palette.editor_bg,
                                (1.0, theme::LINE()),
                                egui::StrokeKind::Inside,
                            );
                            for (n, color) in [palette.keyword, palette.string, palette.func]
                                .into_iter()
                                .enumerate()
                            {
                                let center =
                                    swatch.left_center() + vec2(10.0 + n as f32 * 10.0, 0.0);
                                painter.circle_filled(center, 3.2, color);
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if !built_in && icon_button(ui, icons::TRASH, "Remove this theme") {
                                    remove = Some(name.clone());
                                    on_button = true;
                                }
                                if current {
                                    ui.label(RichText::new(icons::CHECK).color(theme::STRING()));
                                }
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    ui.add(egui::Label::new(name.as_str()).selectable(false));
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
                        select = Some(name.clone());
                    }
                }
            }
            self.icon_set_rows(ui, &mut select_icons, &mut remove_icons);
        });
        if let Some(name) = remove_icons {
            self.remove_icon_set(&name);
        } else if let Some(name) = select_icons {
            self.select_icon_set(&name);
        }
        if let Some(name) = remove {
            if let Some(dir) = themes_dir() {
                let _ = std::fs::remove_file(dir.join(format!("{name}.json")));
            }
            self.refresh_themes();
            if self.themes.current == name {
                self.select_theme(&ctx, "Graphite");
            }
        } else if let Some(name) = select {
            self.select_theme(&ctx, &name);
        }
    }

    fn marketplace(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.label(RichText::new("Themes").size(26.0).color(theme::STRONG()));
        let subtitle = if self.themes.total > 0 {
            format!("{} themes on the VS Code Marketplace", self.themes.total)
        } else {
            "From the VS Code Marketplace".to_string()
        };
        ui.label(RichText::new(subtitle).color(theme::COMMENT()));
        ui.add_space(12.0);

        let mut run = false;
        ui.horizontal(|ui| {
            for (label, icons) in [("Colour themes", false), ("File icons", true)] {
                if ui
                    .selectable_label(self.themes.icons_mode == icons, label)
                    .clicked()
                    && self.themes.icons_mode != icons
                {
                    self.themes.icons_mode = icons;
                    self.themes.results.clear();
                    run = true;
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            Frame::NONE
                .fill(theme::tint(8))
                .stroke((1.0, theme::LINE()))
                .corner_radius(8)
                .inner_margin(Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    ui.label(RichText::new(icons::SEARCH).color(theme::COMMENT()));
                    let edit = ui.add(
                        TextEdit::singleline(&mut self.themes.query)
                            .desired_width((ui.available_width() - 60.0).clamp(90.0, 320.0))
                            .frame(Frame::NONE)
                            .hint_text(if self.themes.icons_mode {
                                "Search file icons"
                            } else {
                                "Search themes"
                            }),
                    );
                    run |= edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                });
            ui.add_space(8.0);
            for (label, code) in SORTS {
                if ui
                    .selectable_label(self.themes.sort == code, label)
                    .clicked()
                    && self.themes.sort != code
                {
                    self.themes.sort = code;
                    run = true;
                }
            }
            if self.themes.searching.is_some() {
                ui.spinner();
            }
        });
        if run {
            self.start_search(&ctx);
        }
        for text in [&self.themes.error, &self.themes.message] {
            if !text.is_empty() {
                ui.label(RichText::new(text).small().color(theme::COMMENT()));
            }
        }
        ui.add_space(10.0);

        const GAP: f32 = 12.0;
        const MIN_CARD: f32 = 300.0;
        let mut install = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            // Room on the right for the scroll bar, so it doesn't sit on the cards.
            let width = ui.available_width() - 16.0;
            let columns = (((width + GAP) / (MIN_CARD + GAP)).floor() as usize).max(1);
            let card = (width - GAP * (columns as f32 - 1.0)) / columns as f32;
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            for row in self.themes.results.chunks(columns) {
                ui.horizontal(|ui| {
                    for extension in row {
                        if self.extension_card(ui, extension, card) {
                            install = Some(extension.id.clone());
                        }
                    }
                });
            }
            if self.themes.results.is_empty()
                && self.themes.searching.is_none()
                && self.themes.error.is_empty()
            {
                ui.label(RichText::new("No themes match that search.").color(theme::COMMENT()));
            }
            ui.add_space(20.0);
        });
        if let Some(id) = install {
            self.install_extension(&ctx, &id);
        }
    }

    /// One Marketplace entry. Returns true when its Install button was pressed.
    fn extension_card(&self, ui: &mut Ui, extension: &Extension, width: f32) -> bool {
        const ICON: f32 = 52.0;
        let mut install = false;
        let card = Frame::NONE
            .fill(theme::tint(6))
            .stroke((1.0, theme::LINE()))
            .corner_radius(10)
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                // The cards sit in a horizontal row, so their own content needs a column.
                ui.vertical(|ui| {
                    ui.set_width(width - 30.0);
                    ui.spacing_mut().item_spacing = vec2(10.0, 6.0);
                    ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(vec2(ICON, ICON), Sense::hover());
                        match self.themes.icons.get(&extension.id) {
                            Some(texture) => {
                                let image = egui::Image::new(egui::load::SizedTexture::new(
                                    texture.id(),
                                    rect.size(),
                                ))
                                .corner_radius(10);
                                ui.put(rect, image);
                            }
                            // Until the icon arrives (or if there is none): the theme's initial.
                            None => {
                                ui.painter().rect_filled(rect, 10, theme::tint(14));
                                let initial: String = extension
                                    .name
                                    .chars()
                                    .take(1)
                                    .collect::<String>()
                                    .to_uppercase();
                                ui.painter().text(
                                    rect.center(),
                                    Align2::CENTER_CENTER,
                                    initial,
                                    FontId::proportional(22.0),
                                    theme::COMMENT(),
                                );
                            }
                        }
                        ui.vertical(|ui| {
                            ui.add_space(4.0);
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&extension.name)
                                        .size(15.5)
                                        .color(theme::STRONG()),
                                )
                                .selectable(false),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&extension.publisher)
                                        .small()
                                        .color(theme::COMMENT()),
                                )
                                .selectable(false),
                            );
                        });
                    });
                    ui.add(
                        egui::Label::new(
                            RichText::new(&extension.description).color(theme::MUTED()),
                        )
                        .selectable(false),
                    )
                    .on_hover_text(&extension.description);
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let stat = |ui: &mut Ui, icon: char, text: String| {
                            ui.label(
                                RichText::new(format!("{icon} {text}"))
                                    .small()
                                    .color(theme::COMMENT()),
                            );
                        };
                        stat(ui, icons::DOWNLOAD, short_count(extension.installs));
                        if extension.rating > 0.0 {
                            stat(ui, icons::STAR, format!("{:.1}", extension.rating));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let busy = self
                                .themes
                                .installing
                                .as_ref()
                                .is_some_and(|(id, _)| *id == extension.id);
                            if busy {
                                ui.spinner();
                            } else if self.themes.added.contains(&extension.id) {
                                ui.label(
                                    RichText::new(format!("{}  Installed", icons::CHECK))
                                        .small()
                                        .color(theme::STRING()),
                                );
                            } else {
                                let button = egui::Button::new(
                                    RichText::new("Install").color(theme::EDITOR_BG()),
                                )
                                .fill(theme::TEXT())
                                .corner_radius(6);
                                // One install at a time keeps the result unambiguous.
                                install = ui
                                    .add_enabled(self.themes.installing.is_none(), button)
                                    .clicked();
                            }
                        });
                    });
                });
            })
            .response;
        if card.contains_pointer() {
            ui.painter().rect_stroke(
                card.rect,
                10,
                (1.0, theme::tint(60)),
                egui::StrokeKind::Inside,
            );
        }
        install
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marketplace_ids_are_read_from_ids_and_links() {
        let id = Some(("dracula-theme".to_string(), "theme-dracula".to_string()));
        assert_eq!(marketplace_id(" dracula-theme.theme-dracula "), id);
        assert_eq!(
            marketplace_id("https://marketplace.visualstudio.com/items?itemName=dracula-theme.theme-dracula&ssr=false"),
            id
        );
        assert_eq!(marketplace_id("not an id"), None);
        assert_eq!(marketplace_id("pub.name; rm -rf"), None);
    }

    #[test]
    fn counts_are_shortened() {
        assert_eq!(short_count(20_346_655.0), "20.3M");
        assert_eq!(short_count(51_200.0), "51K");
        assert_eq!(short_count(812.0), "812");
    }

    /// Runs against a real extension package when one is pointed to, e.g.
    /// `RIDE_SAMPLE_VSIX=dracula.vsix cargo test`.
    #[test]
    fn a_real_vsix_yields_themes() {
        let Some(path) = std::env::var_os("RIDE_SAMPLE_VSIX") else {
            return;
        };
        let themes = themes_in_vsix(std::fs::read(path).unwrap()).unwrap();
        assert!(!themes.is_empty());
        for (label, text) in themes {
            let palette = theme::from_vscode(&theme::parse_jsonc(&text).unwrap());
            println!(
                "{label}: dark={} bg={:?} keyword={:?}",
                palette.dark, palette.editor_bg, palette.keyword
            );
            assert_ne!(
                palette.keyword, palette.text,
                "{label}: keywords should get their own colour"
            );
        }
    }

    /// Talks to the real Marketplace; run with `RIDE_ONLINE_TESTS=1 cargo test`.
    #[test]
    fn the_marketplace_answers_a_search() {
        if std::env::var_os("RIDE_ONLINE_TESTS").is_none() {
            return;
        }
        let (extensions, total) = search("dracula", 4, false).unwrap();
        assert!(total > 0 && !extensions.is_empty());
        let first = &extensions[0];
        println!(
            "{} by {} â€” {} installs, icon: {:?}",
            first.name, first.publisher, first.installs, first.icon_url
        );
        assert!(marketplace_id(&first.id).is_some());
        assert!(first.installs > 0.0);
    }
}
