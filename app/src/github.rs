//! Signing in with GitHub, to have a name and a picture in live editing. GitHub's device
//! flow: ride shows a code, you enter it on github.com, and ride gets a token that reads
//! your public profile and nothing else. The token stays in your Windows profile.

use super::*;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// ride's OAuth app on GitHub (a public id; the device flow needs no secret).
const CLIENT_ID: &str = "Ov23lirxXKIFOE7n0Xm6";

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
pub(super) struct Identity {
    pub(super) token: String,
    pub(super) login: String,
    pub(super) name: String,
    pub(super) avatar_url: String,
}

impl Identity {
    pub(super) fn shown_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.login
        } else {
            &self.name
        }
    }
}

enum Step {
    /// The code to enter on GitHub, and where.
    Code {
        user_code: String,
        url: String,
    },
    Done(Result<Identity, String>),
}

#[derive(Default)]
pub(super) struct GitHub {
    pub(super) me: Option<Identity>,
    /// A sign-in under way: its code once known, and the end of it.
    code: Option<(String, String)>,
    rx: Option<Receiver<Step>>,
    error: String,
    /// Profile pictures by address: loading, loaded, or failed.
    avatars: HashMap<String, Option<egui::TextureHandle>>,
    avatar_rx: Option<(
        Sender<(String, Option<egui::ColorImage>)>,
        Receiver<(String, Option<egui::ColorImage>)>,
    )>,
}

fn store_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("ride").join("github.json"))
}

impl GitHub {
    pub(super) fn load() -> Self {
        // RIDE_GITHUB_AS=LOGIN stands in for a sign-in, to try live editing between two
        // copies of ride without a GitHub account each.
        if let Ok(login) = std::env::var("RIDE_GITHUB_AS") {
            return Self {
                me: Some(Identity {
                    avatar_url: format!("https://github.com/{login}.png"),
                    name: login.clone(),
                    login,
                    token: String::new(),
                }),
                ..Self::default()
            };
        }
        let me = store_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok());
        Self {
            me,
            ..Self::default()
        }
    }

    fn store(&self) {
        let Some(path) = store_path() else {
            return;
        };
        match &self.me {
            Some(me) => {
                if let Ok(text) = serde_json::to_string_pretty(me) {
                    let _ = std::fs::write(path, text);
                }
            }
            None => {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    pub(super) fn configured() -> bool {
        !CLIENT_ID.is_empty()
    }

    fn start(&mut self, ctx: &Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let result = sign_in(&tx, &ctx);
            let _ = tx.send(Step::Done(result));
            ctx.request_repaint();
        });
        self.rx = Some(rx);
        self.code = None;
        self.error.clear();
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else {
            return;
        };
        while let Ok(step) = rx.try_recv() {
            match step {
                Step::Code { user_code, url } => {
                    tools::open_in_browser(&url);
                    self.code = Some((user_code, url));
                }
                Step::Done(result) => {
                    self.rx = None;
                    self.code = None;
                    match result {
                        Ok(me) => {
                            self.me = Some(me);
                            self.store();
                        }
                        Err(error) => self.error = error,
                    }
                    return;
                }
            }
        }
    }

    /// A profile picture, fetched the first time it is asked for.
    pub(super) fn avatar(&mut self, ctx: &Context, url: &str) -> Option<egui::TextureId> {
        if url.is_empty() {
            return None;
        }
        let (tx, rx) = self.avatar_rx.get_or_insert_with(channel);
        while let Ok((url, image)) = rx.try_recv() {
            let texture = image.map(|image| {
                ctx.load_texture(format!("avatar:{url}"), image, egui::TextureOptions::LINEAR)
            });
            self.avatars.insert(url, texture);
        }
        match self.avatars.get(url) {
            Some(texture) => texture.as_ref().map(|t| t.id()),
            None => {
                self.avatars.insert(url.to_string(), None);
                let (tx, url, ctx) = (tx.clone(), url.to_string(), ctx.clone());
                thread::spawn(move || {
                    let sized = format!("{url}{}s=64", if url.contains('?') { "&" } else { "?" });
                    let image = get_bytes(&sized).ok().and_then(|bytes| {
                        let rgba = image::load_from_memory(&bytes)
                            .ok()?
                            .thumbnail(64, 64)
                            .to_rgba8();
                        Some(egui::ColorImage::from_rgba_unmultiplied(
                            [rgba.width() as usize, rgba.height() as usize],
                            rgba.as_raw(),
                        ))
                    });
                    // Not loaded: asked again later.
                    let _ = tx.send((url, image));
                    ctx.request_repaint();
                });
                None
            }
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .user_agent(format!("ride/{}", update::VERSION))
        .build()
        .into()
}

fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    agent()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(2 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

fn post_form(url: &str, form: &[(&str, &str)]) -> Result<serde_json::Value, String> {
    let body = form
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let mut response = agent()
        .post(url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(body.as_str())
        .map_err(|e| format!("GitHub can't be reached: {e}"))?;
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn sign_in(tx: &Sender<Step>, ctx: &Context) -> Result<Identity, String> {
    let start = post_form(
        "https://github.com/login/device/code",
        &[("client_id", CLIENT_ID), ("scope", "")],
    )?;
    let field = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or("").to_string();
    let device_code = field(&start, "device_code");
    if device_code.is_empty() {
        return Err(format!(
            "GitHub said: {}",
            field(&start, "error_description")
        ));
    }
    let _ = tx.send(Step::Code {
        user_code: field(&start, "user_code"),
        url: field(&start, "verification_uri"),
    });
    ctx.request_repaint();
    let mut interval = start["interval"].as_u64().unwrap_or(5).max(1);
    let deadline = std::time::Instant::now()
        + Duration::from_secs(start["expires_in"].as_u64().unwrap_or(900));
    let token = loop {
        std::thread::sleep(Duration::from_secs(interval));
        if std::time::Instant::now() > deadline {
            return Err("The code expired; sign in again.".into());
        }
        let answer = post_form(
            "https://github.com/login/oauth/access_token",
            &[
                ("client_id", CLIENT_ID),
                ("device_code", &device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ],
        )?;
        let token = field(&answer, "access_token");
        if !token.is_empty() {
            break token;
        }
        match field(&answer, "error").as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += 5,
            "access_denied" => return Err("Sign-in was cancelled on GitHub.".into()),
            "expired_token" => return Err("The code expired; sign in again.".into()),
            other => return Err(format!("GitHub said: {other}")),
        }
    };
    let mut response = agent()
        .get("https://api.github.com/user")
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let user: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(Identity {
        token,
        login: field(&user, "login"),
        name: field(&user, "name"),
        avatar_url: field(&user, "avatar_url"),
    })
}

impl App {
    /// Settings → Account: who you are on GitHub, and live editing.
    pub(super) fn account_rows(&mut self, ui: &mut Ui, live: &mut bool) {
        self.github.poll();
        let ctx = ui.ctx().clone();
        match self.github.me.clone() {
            Some(me) => {
                ui.horizontal(|ui| {
                    if let Some(texture) = self.github.avatar(&ctx, &me.avatar_url) {
                        ui.add(
                            egui::Image::new(egui::load::SizedTexture::new(
                                texture,
                                vec2(40.0, 40.0),
                            ))
                            .corner_radius(20),
                        );
                    }
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new(me.shown_name())
                                .size(16.0)
                                .color(theme::STRONG()),
                        );
                        ui.label(RichText::new(format!("@{}", me.login)).color(theme::COMMENT()));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Sign out").clicked() {
                            self.github.me = None;
                            self.github.store();
                        }
                    });
                });
            }
            None if !GitHub::configured() => {
                ui.label(
                    RichText::new("Signing in with GitHub isn't set up in this build of ride.")
                        .color(theme::COMMENT()),
                );
            }
            None => match self.github.code.clone() {
                Some((code, url)) => {
                    ui.label("Enter this code on GitHub, in the page that just opened:");
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(&code)
                                .size(26.0)
                                .monospace()
                                .color(theme::STRONG()),
                        );
                        if ui.button(format!("{}  Copy", icons::COPY)).clicked() {
                            ui.ctx().copy_text(code.clone());
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Waiting for GitHub…").color(theme::COMMENT()));
                        if ui.link(&url).clicked() {
                            tools::open_in_browser(&url);
                        }
                    });
                }
                None if self.github.rx.is_some() => {
                    ui.spinner();
                }
                None => {
                    if ui.button("Sign in with GitHub").clicked() {
                        self.github.start(&ctx);
                    }
                    if !self.github.error.is_empty() {
                        ui.label(
                            RichText::new(&self.github.error)
                                .small()
                                .color(theme::ANSI()[1]),
                        );
                    }
                }
            },
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 300.0).max(160.0));
                ui.label(RichText::new("Live editing").color(theme::STRONG()));
                ui.label(
                    RichText::new(
                        "When someone else has the same file open on the same server through ride, you edit it together and see each other's cursor. Needs you signed in.",
                    )
                    .small()
                    .color(theme::COMMENT()),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_enabled(self.github.me.is_some(), egui::Checkbox::without_text(live));
            });
        });
    }
}
