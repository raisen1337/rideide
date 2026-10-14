//! The database explorer: MySQL, MariaDB and PostgreSQL databases reached from the host,
//! through the `mysql`/`mariadb` and `psql` clients there. Nothing is opened from this
//! computer; the password and the SQL go to the client on its input, so neither shows up
//! in the host's process list.

use super::*;
use crate::ai::JobKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Default, Debug)]
pub(super) enum Engine {
    #[default]
    MySql,
    Postgres,
}

impl Engine {
    fn label(self) -> &'static str {
        match self {
            Engine::MySql => "MySQL / MariaDB",
            Engine::Postgres => "PostgreSQL",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Engine::MySql => 3306,
            Engine::Postgres => 5432,
        }
    }
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub(super) struct Conn {
    /// The ride host it belongs to.
    pub(super) on: String,
    pub(super) name: String,
    pub(super) engine: Engine,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) user: String,
    /// Kept on disk only with `remember`; otherwise for this run of ride.
    pub(super) password: String,
    pub(super) database: String,
    pub(super) remember: bool,
}

impl Conn {
    fn title(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }
        let db = if self.database.is_empty() {
            "(no database)"
        } else {
            &self.database
        };
        format!("{db} · {}@{}", self.user, self.host)
    }

    /// The client command; it reads the password from its first input line, then the SQL.
    fn command(&self) -> String {
        let q = |s: &str| tools::quote(s);
        match self.engine {
            Engine::MySql => {
                let db = if self.database.is_empty() {
                    String::new()
                } else {
                    format!(" {}", q(&self.database))
                };
                format!(
                    "IFS= read -r MYSQL_PWD; export MYSQL_PWD; c=$(command -v mysql || command -v mariadb) || {{ echo 'No mysql or mariadb client on this host' >&2; exit 127; }}; exec \"$c\" -h {} -P {} -u {} --batch --default-character-set=utf8mb4 --connect-timeout=10{db}",
                    q(&self.host),
                    self.port,
                    q(&self.user),
                )
            }
            Engine::Postgres => format!(
                "IFS= read -r PGPASSWORD; export PGPASSWORD PGCONNECT_TIMEOUT=10; command -v psql >/dev/null || {{ echo 'No psql client on this host' >&2; exit 127; }}; exec psql -h {} -p {} -U {} -d {} -X -q --csv -P null='\\N' -v ON_ERROR_STOP=1",
                q(&self.host),
                self.port,
                q(&self.user),
                q(if self.database.is_empty() { "postgres" } else { &self.database }),
            ),
        }
    }

    fn tables_sql(&self) -> &'static str {
        match self.engine {
            Engine::MySql => "SELECT table_name, table_type FROM information_schema.tables WHERE table_schema = DATABASE() ORDER BY table_name;",
            Engine::Postgres => "SELECT table_schema || '.' || table_name, table_type FROM information_schema.tables WHERE table_schema NOT IN ('pg_catalog', 'information_schema') ORDER BY 1;",
        }
    }

    fn columns_sql(&self, table: &str) -> String {
        let lit = |s: &str| format!("'{}'", s.replace('\'', "''"));
        match self.engine {
            Engine::MySql => format!(
                "SELECT column_name, column_type, column_key FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = {} ORDER BY ordinal_position;",
                lit(table)
            ),
            Engine::Postgres => {
                let (schema, name) = table.split_once('.').unwrap_or(("public", table));
                format!(
                    "SELECT column_name, data_type, '' FROM information_schema.columns WHERE table_schema = {} AND table_name = {} ORDER BY ordinal_position;",
                    lit(schema),
                    lit(name)
                )
            }
        }
    }

    fn browse_sql(&self, table: &str) -> String {
        match self.engine {
            Engine::MySql => format!("SELECT * FROM `{}` LIMIT 200;", table.replace('`', "``")),
            Engine::Postgres => {
                let quoted: Vec<String> = table
                    .split('.')
                    .map(|part| format!("\"{}\"", part.replace('"', "\"\"")))
                    .collect();
                format!("SELECT * FROM {} LIMIT 200;", quoted.join("."))
            }
        }
    }
}

/// A result: column names and rows, `None` for NULL.
#[derive(Default, Clone, Debug, PartialEq)]
pub(super) struct Grid {
    pub(super) columns: Vec<String>,
    pub(super) rows: Vec<Vec<Option<String>>>,
    /// More rows came back than are kept.
    pub(super) cut: bool,
}

const MAX_ROWS: usize = 2000;

/// `mysql --batch`: tab-separated, with tabs, newlines and backslashes escaped.
fn parse_mysql(output: &str) -> Grid {
    let unescape = |cell: &str| -> Option<String> {
        if cell == "NULL" {
            return None;
        }
        let mut out = String::with_capacity(cell.len());
        let mut chars = cell.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        }
        Some(out)
    };
    let mut lines = output.lines();
    let Some(header) = lines.next() else {
        return Grid::default();
    };
    let mut grid = Grid {
        columns: header.split('\t').map(str::to_string).collect(),
        ..Grid::default()
    };
    for line in lines {
        if grid.rows.len() == MAX_ROWS {
            grid.cut = true;
            break;
        }
        grid.rows.push(line.split('\t').map(unescape).collect());
    }
    grid
}

/// `psql --csv`: RFC 4180, with `\N` for NULL.
fn parse_csv(output: &str) -> Grid {
    let mut records: Vec<Vec<Option<String>>> = Vec::new();
    let mut record = Vec::new();
    let (mut cell, mut quoted, mut in_quotes) = (String::new(), false, false);
    let mut chars = output.chars().peekable();
    let end_cell = |record: &mut Vec<Option<String>>, cell: &mut String, quoted: &mut bool| {
        let value = std::mem::take(cell);
        record.push(if !*quoted && value == "\\N" {
            None
        } else {
            Some(value)
        });
        *quoted = false;
    };
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                '"' => in_quotes = false,
                _ => cell.push(c),
            }
            continue;
        }
        match c {
            '"' => {
                in_quotes = true;
                quoted = true;
            }
            ',' => end_cell(&mut record, &mut cell, &mut quoted),
            '\r' => {}
            '\n' => {
                end_cell(&mut record, &mut cell, &mut quoted);
                records.push(std::mem::take(&mut record));
                if records.len() > MAX_ROWS + 1 {
                    break;
                }
            }
            _ => cell.push(c),
        }
    }
    if !cell.is_empty() || !record.is_empty() {
        end_cell(&mut record, &mut cell, &mut quoted);
        records.push(record);
    }
    let mut records = records.into_iter();
    let columns = records
        .next()
        .map(|header| header.into_iter().map(Option::unwrap_or_default).collect())
        .unwrap_or_default();
    let mut rows: Vec<_> = records.collect();
    let cut = rows.len() > MAX_ROWS;
    rows.truncate(MAX_ROWS);
    Grid { columns, rows, cut }
}

fn parse(engine: Engine, output: &str) -> Grid {
    match engine {
        Engine::MySql => parse_mysql(output),
        Engine::Postgres => parse_csv(output),
    }
}

/// Connection settings read from a project's `.env`: `DATABASE_URL`, or the usual
/// `DB_*`, `MYSQL_*` and `PG*` variables.
pub(super) fn from_env(text: &str) -> Option<Conn> {
    let vars: HashMap<String, String> = envfile::pairs(text).into_iter().collect();
    let get = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| vars.get(*k).filter(|v| !v.is_empty()).cloned())
    };
    if let Some(url) = get(&["DATABASE_URL", "DB_URL", "MYSQL_URL", "POSTGRES_URL"]) {
        if let Some(conn) = from_url(&url) {
            return Some(conn);
        }
    }
    let kind = get(&["DB_CONNECTION", "DB_DRIVER", "DB_TYPE", "DB_DIALECT"]).unwrap_or_default();
    let postgres = kind.contains("pg")
        || kind.contains("postgres")
        || get(&["PGHOST", "POSTGRES_USER", "PGUSER"]).is_some();
    let engine = if postgres {
        Engine::Postgres
    } else {
        Engine::MySql
    };
    let host = get(&["DB_HOST", "MYSQL_HOST", "PGHOST", "POSTGRES_HOST"]);
    let user = get(&[
        "DB_USERNAME",
        "DB_USER",
        "MYSQL_USER",
        "PGUSER",
        "POSTGRES_USER",
    ]);
    if host.is_none() && user.is_none() {
        return None;
    }
    Some(Conn {
        engine,
        host: host.unwrap_or_else(|| "127.0.0.1".into()),
        port: get(&["DB_PORT", "MYSQL_PORT", "PGPORT", "POSTGRES_PORT"])
            .and_then(|p| p.parse().ok())
            .unwrap_or(engine.default_port()),
        user: user.unwrap_or_default(),
        password: get(&[
            "DB_PASSWORD",
            "DB_PASS",
            "MYSQL_PASSWORD",
            "PGPASSWORD",
            "POSTGRES_PASSWORD",
        ])
        .unwrap_or_default(),
        database: get(&[
            "DB_DATABASE",
            "DB_NAME",
            "MYSQL_DATABASE",
            "PGDATABASE",
            "POSTGRES_DB",
        ])
        .unwrap_or_default(),
        ..Conn::default()
    })
}

/// `mysql://user:pass@host:3306/db` and `postgres://…`.
fn from_url(url: &str) -> Option<Conn> {
    let (scheme, rest) = url.split_once("://")?;
    let engine = match scheme {
        "mysql" | "mariadb" | "mysql2" => Engine::MySql,
        "postgres" | "postgresql" | "pgsql" => Engine::Postgres,
        _ => return None,
    };
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let (auth, place) = rest.rsplit_once('@').unwrap_or(("", rest));
    let (user, password) = auth.split_once(':').unwrap_or((auth, ""));
    let (hostport, database) = place.split_once('/').unwrap_or((place, ""));
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()),
        None => (hostport, None),
    };
    let decode = |s: &str| {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    Some(Conn {
        engine,
        host: if host.is_empty() {
            "127.0.0.1".into()
        } else {
            host.into()
        },
        port: port.unwrap_or(engine.default_port()),
        user: decode(user),
        password: decode(password),
        database: decode(database),
        ..Conn::default()
    })
}

fn store_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(base).join("ride").join("databases.json"))
}

fn load() -> Vec<Conn> {
    store_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn store(conns: &[Conn]) {
    let kept: Vec<Conn> = conns
        .iter()
        .map(|c| Conn {
            password: if c.remember {
                c.password.clone()
            } else {
                String::new()
            },
            ..c.clone()
        })
        .collect();
    if let (Some(path), Ok(text)) = (store_path(), serde_json::to_string_pretty(&kept)) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, text);
    }
}

/// What a database command was for.
pub(super) enum Job {
    Tables,
    Columns(String),
    /// A query tab's run, and the connection it ran on.
    Query {
        tab_uid: u64,
        engine: Engine,
    },
}

/// A query tab: its connection, its SQL, and what the last run gave.
pub(super) struct QueryTab {
    conn: usize,
    pub(super) sql: String,
    result: Option<Result<Grid, String>>,
    running: bool,
    started: f64,
    took: f64,
    /// Column widths, measured when a result arrives.
    widths: Vec<f32>,
}

pub(super) struct Db {
    conns: Vec<Conn>,
    selected: Option<usize>,
    tables: Vec<(String, bool)>,
    columns: HashMap<String, Vec<(String, String, String)>>,
    expanded: HashSet<String>,
    filter: String,
    /// The connection being added or changed, and its place in the list (None: a new one).
    form: Option<(Conn, Option<usize>)>,
    loading: bool,
    error: String,
    queries: usize,
}

impl Default for Db {
    fn default() -> Self {
        Self {
            conns: load(),
            selected: None,
            tables: Vec::new(),
            columns: HashMap::new(),
            expanded: HashSet::new(),
            filter: String::new(),
            form: None,
            loading: false,
            error: String::new(),
            queries: 0,
        }
    }
}

/// The query tab: the SQL, Run, and the result grid. True when Run was asked for.
pub(super) fn query_tab(ui: &mut Ui, tab: &mut QueryTab) -> bool {
    let mut run = false;
    // Column widths are measured once per result, from its first rows.
    if let Some(Ok(grid)) = &tab.result {
        if tab.widths.len() != grid.columns.len() {
            tab.widths = measure(ui, grid);
        }
    }
    Frame::NONE
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            let edit = ui.add(
                TextEdit::multiline(&mut tab.sql)
                    .code_editor()
                    .desired_rows(6)
                    .desired_width(f32::INFINITY)
                    .hint_text("SELECT …"),
            );
            run |=
                edit.has_focus() && ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter));
            ui.horizontal(|ui| {
                let button = egui::Button::new(
                    RichText::new(format!("{}  Run", icons::PLAY)).color(theme::EDITOR_BG()),
                )
                .fill(theme::TEXT());
                run |= ui
                    .add_enabled(!tab.running && !tab.sql.trim().is_empty(), button)
                    .on_hover_text("Ctrl+Enter")
                    .clicked();
                if tab.running {
                    ui.spinner();
                    ui.label(RichText::new("Running…").small().color(theme::COMMENT()));
                }
                match &tab.result {
                    Some(Ok(grid)) if !tab.running => {
                        let rows = grid.rows.len();
                        let note = if grid.columns.is_empty() || grid.columns == [""] {
                            format!("Done in {:.0} ms", tab.took * 1000.0)
                        } else {
                            format!(
                                "{rows}{} row{} in {:.0} ms",
                                if grid.cut { "+" } else { "" },
                                if rows == 1 { "" } else { "s" },
                                tab.took * 1000.0
                            )
                        };
                        ui.label(RichText::new(note).small().color(theme::COMMENT()));
                    }
                    _ => {}
                }
            });
        });
    match &tab.result {
        Some(Err(error)) => {
            Frame::NONE
                .inner_margin(Margin::symmetric(12, 4))
                .show(ui, |ui| {
                    ui.label(RichText::new(error).monospace().color(theme::ANSI()[1]));
                });
        }
        Some(Ok(grid)) if !grid.columns.is_empty() && grid.columns != [""] => {
            grid_ui(ui, grid, &tab.widths);
        }
        _ => {}
    }
    run
}

fn measure(ui: &Ui, grid: &Grid) -> Vec<f32> {
    let font = FontId::monospace(12.5);
    let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&font, 'm'));
    (0..grid.columns.len())
        .map(|c| {
            let longest = grid
                .rows
                .iter()
                .take(200)
                .map(|r| {
                    r.get(c)
                        .and_then(|v| v.as_deref())
                        .map_or(4, |v| v.chars().count())
                })
                .chain(std::iter::once(grid.columns[c].chars().count()))
                .max()
                .unwrap_or(4);
            (longest.min(48) as f32 * cw + 18.0).max(60.0)
        })
        .collect()
}

/// Rows of a result, the header staying on top while they scroll; a click copies a cell.
fn grid_ui(ui: &mut Ui, grid: &Grid, widths: &[f32]) {
    let font = FontId::monospace(12.5);
    let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font)) + 8.0;
    let total: f32 = widths.iter().sum::<f32>() + 48.0;
    ScrollArea::horizontal()
        .id_salt("grid_h")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.set_min_width(total);
            let cell = |ui: &mut Ui, width: f32, text: RichText, fill: Option<Color32>| {
                let (rect, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                if let Some(fill) = fill {
                    ui.painter().rect_filled(rect, 0.0, fill);
                }
                let galley = egui::WidgetText::from(text).into_galley(
                    ui,
                    Some(egui::TextWrapMode::Truncate),
                    width - 12.0,
                    egui::TextStyle::Monospace,
                );
                ui.painter().galley(
                    rect.left_center() + vec2(6.0, -galley.size().y / 2.0),
                    galley,
                    theme::TEXT(),
                );
                resp
            };
            // The header.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                cell(
                    ui,
                    48.0,
                    RichText::new("#")
                        .font(font.clone())
                        .color(theme::COMMENT()),
                    Some(theme::tint(10)),
                );
                for (name, width) in grid.columns.iter().zip(widths) {
                    cell(
                        ui,
                        *width,
                        RichText::new(name)
                            .font(font.clone())
                            .strong()
                            .color(theme::STRONG()),
                        Some(theme::tint(10)),
                    );
                }
            });
            ScrollArea::vertical()
                .id_salt("grid_v")
                .auto_shrink(false)
                .show_rows(ui, row_h, grid.rows.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for index in range {
                        let row = &grid.rows[index];
                        let stripe = (index % 2 == 1).then(|| theme::tint(4));
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            cell(
                                ui,
                                48.0,
                                RichText::new((index + 1).to_string())
                                    .font(font.clone())
                                    .color(theme::COMMENT()),
                                stripe,
                            );
                            for (c, width) in widths.iter().enumerate() {
                                let value = row.get(c).cloned().flatten();
                                let text = match &value {
                                    None => RichText::new("NULL")
                                        .font(font.clone())
                                        .italics()
                                        .color(theme::COMMENT()),
                                    Some(v) => RichText::new(v.replace('\n', "⏎"))
                                        .font(font.clone())
                                        .color(theme::TEXT()),
                                };
                                let resp = cell(ui, *width, text, stripe);
                                if let Some(v) = &value {
                                    let resp = resp.on_hover_text(v.as_str());
                                    if resp.clicked() {
                                        ui.ctx().copy_text(v.clone());
                                    }
                                }
                            }
                        });
                    }
                });
        });
}

impl App {
    fn db_conn(&self) -> Option<&Conn> {
        self.db.selected.and_then(|i| self.db.conns.get(i))
    }

    /// Runs SQL on a connection; the client gets the password, then the SQL, on its input.
    fn db_run(&mut self, conn: &Conn, sql: &str, job: Job) -> bool {
        if chat::local_shell() {
            self.db.error =
                "The database explorer works on Linux hosts, through their mysql or psql client."
                    .into();
            return false;
        }
        let Some(proc) = self.start_shell_job(JobKind::Db(job), conn.command()) else {
            return false;
        };
        let input = format!("{}\n{sql}\n", conn.password);
        self.request(Request::ProcInput {
            proc,
            data: input.into_bytes(),
        });
        self.request(Request::ProcInput {
            proc,
            data: Vec::new(),
        });
        true
    }

    fn db_select(&mut self, index: usize) {
        self.db.selected = Some(index);
        self.db.tables.clear();
        self.db.columns.clear();
        self.db.expanded.clear();
        self.db.error.clear();
        if let Some(conn) = self.db.conns.get(index).cloned() {
            self.db.loading = self.db_run(&conn, conn.tables_sql(), Job::Tables);
        }
    }

    /// Opens a query tab on the selected connection, with some SQL, run at once if asked.
    fn db_query_tab(&mut self, sql: String, run: bool) {
        let Some(conn) = self.db.selected else {
            return;
        };
        self.db.queries += 1;
        let title = format!("SQL {} — {}", self.db.queries, self.db.conns[conn].title());
        let query = db::QueryTab {
            conn,
            sql,
            result: None,
            running: false,
            started: 0.0,
            took: 0.0,
            widths: Vec::new(),
        };
        self.push_preview_tab(title.clone(), preview::Preview::Query(Box::new(query)));
        if run {
            if let Some(index) = self.tabs.iter().position(|t| t.path == title) {
                self.run_query(index);
            }
        }
    }

    /// RIDE_DB_GRID opens a query tab holding a made-up result, to look at the grid
    /// without a database.
    pub(super) fn db_grid_from_env(&mut self) {
        if std::env::var_os("RIDE_DB_GRID").is_none() {
            return;
        }
        let mut sample = String::from("id\tname\temail\tcreated_at\tnote\n");
        for i in 1..=60 {
            let note = if i % 4 == 0 {
                "NULL".to_string()
            } else {
                format!("line one\\nline two {i}")
            };
            sample.push_str(&format!(
                "{i}\tUser {i}\tuser{i}@example.com\t2026-10-{:02} 12:00:00\t{note}\n",
                i % 28 + 1
            ));
        }
        let query = QueryTab {
            conn: 0,
            sql: "SELECT * FROM `users` LIMIT 200;".into(),
            result: Some(Ok(parse_mysql(&sample))),
            running: false,
            started: 0.0,
            took: 0.012,
            widths: Vec::new(),
        };
        self.push_preview_tab(
            "SQL 1 — demo".into(),
            preview::Preview::Query(Box::new(query)),
        );
    }

    pub(super) fn run_query(&mut self, index: usize) {
        let uid = self.tabs[index].uid;
        let Some(preview::Preview::Query(query)) = &mut self.tabs[index].preview else {
            return;
        };
        let Some(conn) = self.db.conns.get(query.conn).cloned() else {
            return;
        };
        let sql = query.sql.clone();
        query.running = true;
        query.started = self.now;
        let job = Job::Query {
            tab_uid: uid,
            engine: conn.engine,
        };
        if !self.db_run(&conn, &sql, job) {
            if let Some(preview::Preview::Query(query)) = &mut self.tabs[index].preview {
                query.running = false;
                query.result = Some(Err(self.db.error.clone()));
            }
        }
    }

    pub(super) fn db_done(&mut self, job: Job, outcome: Result<String, String>) {
        let engine = self.db_conn().map_or(Engine::MySql, |c| c.engine);
        match job {
            Job::Tables => {
                self.db.loading = false;
                match outcome {
                    Ok(output) => {
                        let grid = parse(engine, &output);
                        self.db.tables = grid
                            .rows
                            .into_iter()
                            .filter_map(|r| {
                                let name = r.first()?.clone()?;
                                let view = r
                                    .get(1)
                                    .cloned()
                                    .flatten()
                                    .is_some_and(|t| t.contains("VIEW"));
                                Some((name, view))
                            })
                            .collect();
                        self.db.error.clear();
                    }
                    Err(error) => self.db.error = error.trim().to_string(),
                }
            }
            Job::Columns(table) => {
                if let Ok(output) = outcome {
                    let grid = parse(engine, &output);
                    let columns = grid
                        .rows
                        .into_iter()
                        .map(|r| {
                            let at = |i: usize| r.get(i).cloned().flatten().unwrap_or_default();
                            (at(0), at(1), at(2))
                        })
                        .collect();
                    self.db.columns.insert(table, columns);
                }
            }
            Job::Query { tab_uid, engine } => {
                let now = self.now;
                let Some(tab) = self.tabs.iter_mut().find(|t| t.uid == tab_uid) else {
                    return;
                };
                let Some(preview::Preview::Query(query)) = &mut tab.preview else {
                    return;
                };
                query.running = false;
                query.took = now - query.started;
                query.result = Some(match outcome {
                    Ok(output) => Ok(parse(engine, &output)),
                    Err(error) => Err(error.trim().to_string()),
                });
                query.widths.clear();
            }
        }
    }

    /// The Database side tab: the connections of this host, and the tables of the chosen one.
    pub(super) fn database_panel(&mut self, ui: &mut Ui) {
        if self.db.form.is_some() {
            self.db_form(ui);
            return;
        }
        let host = self.host.trim().to_string();
        let mine: Vec<usize> = (0..self.db.conns.len())
            .filter(|&i| self.db.conns[i].on == host)
            .collect();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("DATABASES")
                    .small()
                    .strong()
                    .color(theme::COMMENT()),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, icons::ADD, "Add a connection") {
                    self.db.form = Some((
                        Conn {
                            on: host.clone(),
                            host: "127.0.0.1".into(),
                            port: 3306,
                            ..Conn::default()
                        },
                        None,
                    ));
                }
                if self.db.selected.is_some()
                    && icon_button(ui, icons::REFRESH, "Reload the tables")
                {
                    if let Some(index) = self.db.selected {
                        self.db_select(index);
                    }
                }
            });
        });
        if mine.is_empty() {
            ui.add_space(6.0);
            ui.label(
                RichText::new("No database connections on this host yet.").color(theme::COMMENT()),
            );
            ui.add_space(4.0);
            if ui
                .button(format!("{}  Add a connection", icons::ADD))
                .clicked()
            {
                self.db.form = Some((
                    Conn {
                        on: host,
                        host: "127.0.0.1".into(),
                        port: 3306,
                        ..Conn::default()
                    },
                    None,
                ));
            }
            return;
        }
        let mut select = None;
        let mut edit = None;
        for &index in &mine {
            let conn = &self.db.conns[index];
            let chosen = self.db.selected == Some(index);
            ui.horizontal(|ui| {
                ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, icons::EDIT, "Change") {
                        edit = Some(index);
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        let label = pieces(&[
                            (&format!("{}  ", icons::DATABASE), theme::COMMENT()),
                            (&conn.title(), theme::TEXT()),
                        ]);
                        if ui.selectable_label(chosen, label).clicked() {
                            select = Some(index);
                        }
                    });
                });
            });
        }
        if let Some(index) = select {
            self.db_select(index);
        }
        if let Some(index) = edit {
            self.db.form = Some((self.db.conns[index].clone(), Some(index)));
        }
        let Some(selected) = self.db.selected.filter(|i| mine.contains(i)) else {
            return;
        };
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(format!("{}  New query", icons::ADD)).clicked() {
                self.db_query_tab(String::new(), false);
            }
            if self.db.loading {
                ui.spinner();
            }
        });
        if !self.db.error.is_empty() {
            ui.label(
                RichText::new(&self.db.error)
                    .small()
                    .color(theme::ANSI()[1]),
            );
        }
        ui.add_space(4.0);
        Frame::NONE
            .fill(theme::tint(8))
            .corner_radius(6)
            .inner_margin(Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icons::SEARCH).color(theme::COMMENT()));
                    ui.add(
                        TextEdit::singleline(&mut self.db.filter)
                            .desired_width(f32::INFINITY)
                            .frame(Frame::NONE)
                            .hint_text(format!("Filter {} tables", self.db.tables.len())),
                    );
                });
            });
        let filter = self.db.filter.to_lowercase();
        let conn = self.db.conns[selected].clone();
        let mut browse = None;
        let mut toggle = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (name, view) in &self.db.tables {
                if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                    continue;
                }
                let open = self.db.expanded.contains(name);
                ui.horizontal(|ui| {
                    let chevron = if open {
                        icons::CHEVRON_DOWN
                    } else {
                        icons::CHEVRON_RIGHT
                    };
                    if ui
                        .add(
                            egui::Button::new(RichText::new(chevron).color(theme::COMMENT()))
                                .frame(false),
                        )
                        .on_hover_text("Columns")
                        .clicked()
                    {
                        toggle = Some(name.clone());
                    }
                    let kind = if *view { "view" } else { "" };
                    let label = pieces(&[
                        (name, theme::TEXT()),
                        (&format!("  {kind}"), theme::COMMENT()),
                    ]);
                    if ui
                        .selectable_label(false, label)
                        .on_hover_text("Show the first 200 rows")
                        .clicked()
                    {
                        browse = Some(name.clone());
                    }
                });
                if open {
                    match self.db.columns.get(name) {
                        Some(columns) => {
                            for (column, kind, key) in columns {
                                ui.horizontal(|ui| {
                                    ui.add_space(30.0);
                                    let key = if key.is_empty() {
                                        String::new()
                                    } else {
                                        format!("  {key}")
                                    };
                                    ui.label(pieces(&[
                                        (column, theme::TEXT()),
                                        (&format!("  {kind}"), theme::COMMENT()),
                                        (&key, theme::ACCENT()),
                                    ]));
                                });
                            }
                        }
                        None => {
                            ui.horizontal(|ui| {
                                ui.add_space(30.0);
                                ui.spinner();
                            });
                        }
                    }
                }
            }
        });
        if let Some(table) = toggle {
            if !self.db.expanded.remove(&table) {
                self.db.expanded.insert(table.clone());
                if !self.db.columns.contains_key(&table) {
                    let sql = conn.columns_sql(&table);
                    self.db_run(&conn, &sql, Job::Columns(table));
                }
            }
        }
        if let Some(table) = browse {
            self.db_query_tab(conn.browse_sql(&table), true);
        }
    }

    /// Adding or changing a connection.
    fn db_form(&mut self, ui: &mut Ui) {
        let Some((mut conn, place)) = self.db.form.take() else {
            return;
        };
        let (mut save, mut cancel, mut remove, mut detect) = (false, false, false, false);
        ui.label(
            RichText::new(if place.is_some() {
                "CONNECTION"
            } else {
                "NEW CONNECTION"
            })
            .small()
            .strong()
            .color(theme::COMMENT()),
        );
        ui.add_space(6.0);
        egui::Grid::new("db_form")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Kind");
                egui::ComboBox::from_id_salt("db_engine")
                    .selected_text(conn.engine.label())
                    .show_ui(ui, |ui| {
                        for engine in [Engine::MySql, Engine::Postgres] {
                            if ui
                                .selectable_value(&mut conn.engine, engine, engine.label())
                                .changed()
                            {
                                conn.port = engine.default_port();
                            }
                        }
                    });
                ui.end_row();
                let text = |ui: &mut Ui, label: &str, value: &mut String, password: bool| {
                    ui.label(label);
                    ui.add(
                        TextEdit::singleline(value)
                            .password(password)
                            .desired_width(170.0),
                    );
                    ui.end_row();
                };
                text(ui, "Name", &mut conn.name, false);
                text(ui, "Host", &mut conn.host, false);
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut conn.port).range(1..=65535));
                ui.end_row();
                text(ui, "User", &mut conn.user, false);
                text(ui, "Password", &mut conn.password, true);
                text(ui, "Database", &mut conn.database, false);
            });
        ui.checkbox(&mut conn.remember, "Remember the password on this computer")
            .on_hover_text("Kept in your Windows profile, readable by your account only. Otherwise it is asked again after ride restarts.");
        ui.label(
            RichText::new("The host and port are as seen from the server ride is connected to, so 127.0.0.1 is the database on that server.")
                .small()
                .color(theme::COMMENT()),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            save = ui.button("Save").clicked();
            cancel = ui.button("Cancel").clicked();
            detect = ui
                .button("Fill in from .env")
                .on_hover_text("Reads DATABASE_URL or the DB_ variables of the open folder's .env")
                .clicked();
            if place.is_some() {
                remove = ui.button("Remove").clicked();
            }
        });
        if detect {
            let path = join(&self.root, ".env");
            self.request_then(Request::ReadFile { path }, Pending::DbEnv);
        }
        if save {
            let index = match place {
                Some(index) => {
                    self.db.conns[index] = conn;
                    index
                }
                None => {
                    self.db.conns.push(conn);
                    self.db.conns.len() - 1
                }
            };
            store(&self.db.conns);
            self.db_select(index);
        } else if remove {
            if let Some(index) = place {
                self.db.conns.remove(index);
                self.db.selected = None;
                store(&self.db.conns);
            }
        } else if !cancel {
            self.db.form = Some((conn, place));
        }
    }

    /// The open folder's `.env`, read for the connection form.
    pub(super) fn db_env_read(&mut self, data: &[u8]) {
        let Some((conn, _)) = &mut self.db.form else {
            return;
        };
        match from_env(&String::from_utf8_lossy(data)) {
            Some(found) => {
                *conn = Conn {
                    on: conn.on.clone(),
                    name: conn.name.clone(),
                    remember: conn.remember,
                    ..found
                };
                self.status = "Filled in from .env".into();
            }
            None => self.status = "The .env has no database settings ride recognises".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_batch_output_is_read() {
        let grid = parse_mysql("id\tname\tnote\n1\tAna\tNULL\n2\tline\\none\\ttab\tx\\\\y\n");
        assert_eq!(grid.columns, ["id", "name", "note"]);
        assert_eq!(grid.rows[0][2], None);
        assert_eq!(grid.rows[1][1].as_deref(), Some("line\none\ttab"));
        assert_eq!(grid.rows[1][2].as_deref(), Some("x\\y"));
    }

    #[test]
    fn psql_csv_output_is_read() {
        let grid = parse_csv("id,name,note\n1,\"Pop, Ana\",\\N\n2,\"two\nlines \"\"q\"\"\",\"\"\n");
        assert_eq!(grid.columns, ["id", "name", "note"]);
        assert_eq!(grid.rows[0][1].as_deref(), Some("Pop, Ana"));
        assert_eq!(grid.rows[0][2], None);
        assert_eq!(grid.rows[1][1].as_deref(), Some("two\nlines \"q\""));
        assert_eq!(grid.rows[1][2].as_deref(), Some(""));
    }

    #[test]
    fn connections_are_found_in_env_files() {
        let url = from_env("DATABASE_URL=postgres://app:p%40ss@db.local:5433/shop?sslmode=disable")
            .unwrap();
        assert_eq!(
            (url.engine, url.host.as_str(), url.port),
            (Engine::Postgres, "db.local", 5433)
        );
        assert_eq!(
            (
                url.user.as_str(),
                url.password.as_str(),
                url.database.as_str()
            ),
            ("app", "p@ss", "shop")
        );
        let laravel = from_env("DB_CONNECTION=mysql\nDB_HOST=127.0.0.1\nDB_PORT=3307\nDB_DATABASE=site\nDB_USERNAME=root\nDB_PASSWORD=\"x y\"").unwrap();
        assert_eq!(
            (laravel.engine, laravel.port, laravel.password.as_str()),
            (Engine::MySql, 3307, "x y")
        );
        assert!(from_env("APP_NAME=x").is_none());
    }
}
