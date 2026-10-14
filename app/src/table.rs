//! A table of the database explorer, and the grid that shows query results. A table is
//! read a page at a time, sorted and filtered on the server; its cells are edited in place
//! (double-click), rows are added, duplicated and deleted, and all of it is saved as one
//! transaction. Rows and cells are selected as in a spreadsheet and copied as text, CSV,
//! JSON or SQL.

use super::*;
use crate::db::{Engine, Grid};
use std::collections::{BTreeMap, BTreeSet};

/// A column of a table, as the database describes it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Column {
    pub(super) name: String,
    pub(super) kind: String,
    /// Part of the primary key.
    pub(super) key: bool,
    pub(super) nullable: bool,
    pub(super) default: Option<String>,
    /// Filled in by the database (auto_increment, a sequence, an identity).
    pub(super) auto: bool,
}

/// What a table view needs the app to run.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Want {
    Columns,
    Page,
    Count,
    Save(String),
}

/// A cell of a row being added: left to the column's default, or set.
type NewCell = Option<Option<String>>;

pub(super) struct TableView {
    pub(super) conn: usize,
    pub(super) engine: Engine,
    /// The table, as named in the explorer; empty for a query's result.
    pub(super) table: String,
    pub(super) columns: Vec<Column>,
    /// The rows of the page shown, as they are in the database.
    pub(super) grid: Grid,
    /// Rows that match, all pages together, once counted.
    pub(super) total: Option<u64>,
    pub(super) page: usize,
    pub(super) page_size: usize,
    /// The column sorted by, and whether ascending.
    sort: Option<(usize, bool)>,
    /// A WHERE condition, as typed and as applied.
    filter: String,
    applied_filter: String,
    /// Text looked for in every column, as typed and as applied.
    search: String,
    applied_search: String,
    /// Changed cells of the page's rows, by row and column.
    edits: BTreeMap<(usize, usize), Option<String>>,
    added: Vec<Vec<NewCell>>,
    deleted: BTreeSet<usize>,
    /// The order rows are shown in (a query's result is sorted here, not on the server).
    order: Vec<usize>,
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    /// The cell the keyboard is on, as (row, column).
    cell: Option<(usize, usize)>,
    /// The cell being edited and what is typed in it.
    editing: Option<(usize, usize, String)>,
    focus_editor: bool,
    /// A long value being edited in a box of its own.
    big_edit: Option<(usize, usize, String)>,
    widths: Vec<f32>,
    pub(super) loading: bool,
    pub(super) saving: bool,
    pub(super) error: Option<String>,
    notice: Option<String>,
    pub(super) wants: Vec<Want>,
    pub(super) started: f64,
    pub(super) took: f64,
    /// A query's result: read only, sorted here, all of it on one page.
    local: bool,
    scroll_to: Option<usize>,
}

impl TableView {
    pub(super) fn new(conn: usize, engine: Engine, table: String) -> Self {
        let mut view = Self::empty(conn, engine, table, false);
        view.wants = vec![Want::Columns, Want::Page, Want::Count];
        view.loading = true;
        view
    }

    /// A query's result, to look at, sort and copy.
    pub(super) fn result(grid: Grid) -> Self {
        let mut view = Self::empty(0, Engine::MySql, String::new(), true);
        view.columns = grid
            .columns
            .iter()
            .map(|name| Column {
                name: name.clone(),
                nullable: true,
                ..Column::default()
            })
            .collect();
        view.set_page(grid);
        view
    }

    fn empty(conn: usize, engine: Engine, table: String, local: bool) -> Self {
        Self {
            conn,
            engine,
            table,
            columns: Vec::new(),
            grid: Grid::default(),
            total: None,
            page: 0,
            page_size: 200,
            sort: None,
            filter: String::new(),
            applied_filter: String::new(),
            search: String::new(),
            applied_search: String::new(),
            edits: BTreeMap::new(),
            added: Vec::new(),
            deleted: BTreeSet::new(),
            order: Vec::new(),
            selected: BTreeSet::new(),
            anchor: None,
            cell: None,
            editing: None,
            focus_editor: false,
            big_edit: None,
            widths: Vec::new(),
            loading: false,
            saving: false,
            error: None,
            notice: None,
            wants: Vec::new(),
            started: 0.0,
            took: 0.0,
            local,
            scroll_to: None,
        }
    }

    /// A page of rows arrived: what was changed on the last one is gone with it.
    pub(super) fn set_page(&mut self, grid: Grid) {
        if self.columns.len() != grid.columns.len()
            || self
                .columns
                .iter()
                .zip(&grid.columns)
                .any(|(c, n)| &c.name != n)
        {
            // The columns as the rows have them, with what is known about each.
            let known = std::mem::take(&mut self.columns);
            self.columns = grid
                .columns
                .iter()
                .map(|name| {
                    known
                        .iter()
                        .find(|c| &c.name == name)
                        .cloned()
                        .unwrap_or(Column {
                            name: name.clone(),
                            nullable: true,
                            ..Column::default()
                        })
                })
                .collect();
            self.widths.clear();
        }
        self.grid = grid;
        self.edits.clear();
        self.added.clear();
        self.deleted.clear();
        self.selected.clear();
        self.anchor = None;
        self.editing = None;
        self.cell = None;
        self.loading = false;
        self.reorder();
    }

    pub(super) fn set_columns(&mut self, columns: Vec<Column>) {
        if self.grid.columns.is_empty() {
            self.columns = columns;
            return;
        }
        for column in &mut self.columns {
            if let Some(known) = columns.iter().find(|c| c.name == column.name) {
                *column = known.clone();
            }
        }
    }

    fn base(&self) -> usize {
        self.grid.rows.len()
    }

    fn rows(&self) -> usize {
        self.base() + self.added.len()
    }

    /// What a cell holds now, changes included. `None` is NULL.
    fn value(&self, row: usize, col: usize) -> Option<&str> {
        if row < self.base() {
            match self.edits.get(&(row, col)) {
                Some(value) => value.as_deref(),
                None => self.grid.rows[row].get(col).and_then(|v| v.as_deref()),
            }
        } else {
            self.added
                .get(row - self.base())
                .and_then(|r| r.get(col))
                .and_then(|cell| cell.as_ref())
                .and_then(|v| v.as_deref())
        }
    }

    /// A cell of a new row that is left to its column's default.
    fn is_default(&self, row: usize, col: usize) -> bool {
        row >= self.base()
            && self
                .added
                .get(row - self.base())
                .and_then(|r| r.get(col))
                .is_some_and(|cell| cell.is_none())
    }

    fn original(&self, row: usize, col: usize) -> Option<&str> {
        self.grid.rows.get(row)?.get(col)?.as_deref()
    }

    pub(super) fn set(&mut self, row: usize, col: usize, value: Option<String>) {
        if self.local {
            return;
        }
        if row < self.base() {
            if value.as_deref() == self.original(row, col) {
                self.edits.remove(&(row, col));
            } else {
                self.edits.insert((row, col), value);
            }
        } else if let Some(cells) = self.added.get_mut(row - self.grid.rows.len()) {
            if let Some(cell) = cells.get_mut(col) {
                *cell = Some(value);
            }
        }
    }

    /// How many rows are changed, added or deleted.
    pub(super) fn changes(&self) -> usize {
        let edited: BTreeSet<usize> = self
            .edits
            .keys()
            .map(|(r, _)| *r)
            .filter(|r| !self.deleted.contains(r))
            .collect();
        edited.len() + self.added.len() + self.deleted.len()
    }

    fn reorder(&mut self) {
        self.order = (0..self.rows()).collect();
        if !self.local {
            return;
        }
        if let Some((col, ascending)) = self.sort {
            let grid = &self.grid;
            let get = |r: usize| grid.rows[r].get(col).and_then(|v| v.as_deref());
            self.order.sort_by(|&a, &b| {
                let ordering = compare(get(a), get(b));
                if ascending {
                    ordering
                } else {
                    ordering.reverse()
                }
            });
        }
    }

    fn key_columns(&self) -> Vec<usize> {
        (0..self.columns.len())
            .filter(|&c| self.columns[c].key)
            .collect()
    }

    // ---- SQL ----

    fn ident(&self, name: &str) -> String {
        ident(self.engine, name)
    }

    fn table_ident(&self) -> String {
        match self.engine {
            Engine::MySql => ident(Engine::MySql, &self.table),
            Engine::Postgres => self
                .table
                .split('.')
                .map(|part| ident(Engine::Postgres, part))
                .collect::<Vec<_>>()
                .join("."),
        }
    }

    fn conditions(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if !self.applied_filter.trim().is_empty() {
            parts.push(format!("({})", self.applied_filter.trim()));
        }
        let text = self.applied_search.trim();
        if !text.is_empty() && !self.columns.is_empty() {
            let pattern = literal(self.engine, Some(&format!("%{text}%")));
            let any: Vec<String> = self
                .columns
                .iter()
                .map(|c| match self.engine {
                    Engine::MySql => {
                        format!("CAST({} AS CHAR) LIKE {pattern}", self.ident(&c.name))
                    }
                    Engine::Postgres => {
                        format!("CAST({} AS TEXT) ILIKE {pattern}", self.ident(&c.name))
                    }
                })
                .collect();
            parts.push(format!("({})", any.join(" OR ")));
        }
        parts
    }

    fn where_sql(&self) -> String {
        let parts = self.conditions();
        if parts.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", parts.join(" AND "))
        }
    }

    pub(super) fn page_sql(&self) -> String {
        let order = match self
            .sort
            .and_then(|(c, asc)| Some((self.columns.get(c)?, asc)))
        {
            Some((column, asc)) => format!(
                " ORDER BY {} {}",
                self.ident(&column.name),
                if asc { "ASC" } else { "DESC" }
            ),
            None => String::new(),
        };
        format!(
            "SELECT * FROM {}{}{order} LIMIT {} OFFSET {};",
            self.table_ident(),
            self.where_sql(),
            self.page_size,
            self.page * self.page_size
        )
    }

    pub(super) fn count_sql(&self) -> String {
        format!(
            "SELECT COUNT(*) FROM {}{};",
            self.table_ident(),
            self.where_sql()
        )
    }

    /// The condition that finds a row of the page as it was read: by its primary key,
    /// or, for a table without one, by all of its values.
    fn row_match(&self, row: usize) -> String {
        let keys = self.key_columns();
        let columns: Vec<usize> = if keys.is_empty() {
            (0..self.columns.len()).collect()
        } else {
            keys
        };
        columns
            .iter()
            .map(|&c| {
                let name = self.ident(&self.columns[c].name);
                match self.original(row, c) {
                    None => format!("{name} IS NULL"),
                    Some(v) => format!("{name} = {}", literal(self.engine, Some(v))),
                }
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// The statements that make the changes, in the order they run.
    pub(super) fn save_statements(&self) -> Vec<String> {
        let table = self.table_ident();
        // Without a key, a row is found by its values, and only one of identical rows changes.
        let limit = if self.key_columns().is_empty() && self.engine == Engine::MySql {
            " LIMIT 1"
        } else {
            ""
        };
        let mut out = Vec::new();
        for &row in &self.deleted {
            out.push(format!(
                "DELETE FROM {table} WHERE {}{limit};",
                self.row_match(row)
            ));
        }
        let mut rows: BTreeMap<usize, Vec<(usize, &Option<String>)>> = BTreeMap::new();
        for ((row, col), value) in &self.edits {
            if !self.deleted.contains(row) {
                rows.entry(*row).or_default().push((*col, value));
            }
        }
        for (row, cells) in rows {
            let set: Vec<String> = cells
                .iter()
                .map(|(c, v)| {
                    format!(
                        "{} = {}",
                        self.ident(&self.columns[*c].name),
                        literal(self.engine, v.as_deref())
                    )
                })
                .collect();
            out.push(format!(
                "UPDATE {table} SET {} WHERE {}{limit};",
                set.join(", "),
                self.row_match(row)
            ));
        }
        for cells in &self.added {
            let given: Vec<(usize, &Option<String>)> = cells
                .iter()
                .enumerate()
                .filter_map(|(c, cell)| Some((c, cell.as_ref()?)))
                .collect();
            if given.is_empty() {
                out.push(match self.engine {
                    Engine::MySql => format!("INSERT INTO {table} () VALUES ();"),
                    Engine::Postgres => format!("INSERT INTO {table} DEFAULT VALUES;"),
                });
                continue;
            }
            let names: Vec<String> = given
                .iter()
                .map(|(c, _)| self.ident(&self.columns[*c].name))
                .collect();
            let values: Vec<String> = given
                .iter()
                .map(|(_, v)| literal(self.engine, v.as_deref()))
                .collect();
            out.push(format!(
                "INSERT INTO {table} ({}) VALUES ({});",
                names.join(", "),
                values.join(", ")
            ));
        }
        out
    }

    /// The changes as one transaction: all of them go in, or none.
    pub(super) fn save_sql(&self) -> Option<String> {
        let statements = self.save_statements();
        if statements.is_empty() {
            return None;
        }
        let begin = match self.engine {
            Engine::MySql => "START TRANSACTION;",
            Engine::Postgres => "BEGIN;",
        };
        Some(format!("{begin}\n{}\nCOMMIT;", statements.join("\n")))
    }

    pub(super) fn saved(&mut self) {
        let count = self.changes();
        self.saving = false;
        self.notice = Some(if count == 1 {
            "Saved 1 change".into()
        } else {
            format!("Saved {count} changes")
        });
        self.reload(true);
    }

    /// Reads the page again, and counts again when the rows that match may have changed.
    fn reload(&mut self, count: bool) {
        self.loading = true;
        self.wants.push(Want::Page);
        if count {
            self.wants.push(Want::Count);
        }
    }

    /// Ctrl+S: saves the changes.
    pub(super) fn request_save(&mut self) {
        if let Some(sql) = self.save_sql() {
            self.saving = true;
            self.error = None;
            self.wants.push(Want::Save(sql));
        }
    }

    // ---- copying ----

    /// The rows to copy: the selected ones, or all of them.
    fn rows_for_copy(&self, all: bool) -> Vec<usize> {
        if all || self.selected.is_empty() {
            self.order.clone()
        } else {
            self.order
                .iter()
                .copied()
                .filter(|r| self.selected.contains(r))
                .collect()
        }
    }

    fn text_of(&self, rows: &[usize], header: bool, sep: char) -> String {
        let field = |v: Option<&str>| -> String {
            let v = v.unwrap_or(if sep == ',' { "" } else { "NULL" });
            if sep == ',' && v.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", v.replace('"', "\"\""))
            } else if sep == '\t' {
                v.replace(['\t', '\n'], " ")
            } else {
                v.to_string()
            }
        };
        let mut out = String::new();
        if header {
            let names: Vec<String> = self.columns.iter().map(|c| field(Some(&c.name))).collect();
            out.push_str(&names.join(&sep.to_string()));
            out.push('\n');
        }
        for &row in rows {
            let cells: Vec<String> = (0..self.columns.len())
                .map(|c| field(self.value(row, c)))
                .collect();
            out.push_str(&cells.join(&sep.to_string()));
            out.push('\n');
        }
        out
    }

    fn json_of(&self, rows: &[usize]) -> String {
        let list: Vec<serde_json::Value> = rows
            .iter()
            .map(|&row| {
                let mut object = serde_json::Map::new();
                for (c, column) in self.columns.iter().enumerate() {
                    let value = match self.value(row, c) {
                        None => serde_json::Value::Null,
                        Some(v) => serde_json::Value::String(v.to_string()),
                    };
                    object.insert(column.name.clone(), value);
                }
                serde_json::Value::Object(object)
            })
            .collect();
        serde_json::to_string_pretty(&list).unwrap_or_default()
    }

    fn inserts_of(&self, rows: &[usize]) -> String {
        let table = if self.table.is_empty() {
            self.ident("result")
        } else {
            self.table_ident()
        };
        let names: Vec<String> = self.columns.iter().map(|c| self.ident(&c.name)).collect();
        rows.iter()
            .map(|&row| {
                let values: Vec<String> = (0..self.columns.len())
                    .map(|c| literal(self.engine, self.value(row, c)))
                    .collect();
                format!(
                    "INSERT INTO {table} ({}) VALUES ({});\n",
                    names.join(", "),
                    values.join(", ")
                )
            })
            .collect()
    }

    // ---- changes made from the grid ----

    fn add_row(&mut self, from: Option<usize>) {
        let cells: Vec<NewCell> = (0..self.columns.len())
            .map(|c| match from {
                // A copy leaves out what the database fills in itself (the id).
                Some(row) if !self.columns[c].auto && !self.columns[c].key => {
                    Some(self.value(row, c).map(str::to_string))
                }
                _ => None,
            })
            .collect();
        self.added.push(cells);
        let row = self.rows() - 1;
        self.order.push(row);
        self.selected = BTreeSet::from([row]);
        self.cell = Some((row, 0));
        self.scroll_to = Some(row);
    }

    fn delete_rows(&mut self, rows: &BTreeSet<usize>) {
        let base = self.base();
        // New rows simply go; rows of the page are marked, to be deleted on save.
        let mut gone: Vec<usize> = rows.iter().copied().filter(|&r| r >= base).collect();
        gone.sort_unstable_by(|a, b| b.cmp(a));
        for row in gone {
            if row - base < self.added.len() {
                self.added.remove(row - base);
            }
        }
        for &row in rows.iter().filter(|&&r| r < base) {
            self.deleted.insert(row);
        }
        self.selected.clear();
        self.cell = None;
        self.order = (0..self.rows()).collect();
        if self.local {
            self.reorder();
        }
    }

    fn revert_rows(&mut self, rows: &BTreeSet<usize>) {
        self.edits.retain(|(r, _), _| !rows.contains(r));
        for row in rows {
            self.deleted.remove(row);
        }
    }

    pub(super) fn discard(&mut self) {
        self.edits.clear();
        self.added.clear();
        self.deleted.clear();
        self.editing = None;
        self.selected.clear();
        self.order = (0..self.rows()).collect();
    }

    fn start_edit(&mut self, row: usize, col: usize) {
        if self.local || self.deleted.contains(&row) || col >= self.columns.len() {
            return;
        }
        let value = self.value(row, col).unwrap_or("").to_string();
        if value.contains('\n') || value.chars().count() > 200 {
            self.big_edit = Some((row, col, value));
        } else {
            self.editing = Some((row, col, value));
            self.focus_editor = true;
        }
        self.cell = Some((row, col));
    }

    fn commit_edit(&mut self) {
        if let Some((row, col, text)) = self.editing.take() {
            let unchanged = self.value(row, col) == Some(text.as_str())
                || self.value(row, col).is_none() && text.is_empty() && !self.is_default(row, col);
            if !unchanged {
                self.set(row, col, Some(text));
            }
        }
    }

    /// Moves the cell the keyboard is on.
    fn step(&mut self, rows: isize, cols: isize) {
        if self.order.is_empty() || self.columns.is_empty() {
            return;
        }
        let (row, col) = self.cell.unwrap_or((self.order[0], 0));
        let at = self.order.iter().position(|&r| r == row).unwrap_or(0) as isize;
        let at = (at + rows).clamp(0, self.order.len() as isize - 1) as usize;
        let col = (col as isize + cols).clamp(0, self.columns.len() as isize - 1) as usize;
        let row = self.order[at];
        self.cell = Some((row, col));
        self.scroll_to = Some(row);
    }
}

/// A name in SQL, quoted for the engine.
pub(super) fn ident(engine: Engine, name: &str) -> String {
    match engine {
        Engine::MySql => format!("`{}`", name.replace('`', "``")),
        Engine::Postgres => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// A value in SQL. MySQL reads backslashes in strings as escapes; PostgreSQL doesn't.
pub(super) fn literal(engine: Engine, value: Option<&str>) -> String {
    match value {
        None => "NULL".into(),
        Some(text) => {
            let text = match engine {
                Engine::MySql => text.replace('\\', "\\\\"),
                Engine::Postgres => text.to_string(),
            };
            format!("'{}'", text.replace('\'', "''"))
        }
    }
}

/// Orders values as numbers when both are, as text otherwise; NULLs come first.
fn compare(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    match (a, b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, _) => std::cmp::Ordering::Less,
        (_, None) => std::cmp::Ordering::Greater,
        (Some(a), Some(b)) => match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
            (Ok(x), Ok(y)) => x.total_cmp(&y),
            _ => a.to_lowercase().cmp(&b.to_lowercase()),
        },
    }
}

const NUM_W: f32 = 54.0;

fn measure(ui: &Ui, view: &TableView) -> Vec<f32> {
    let font = FontId::monospace(12.5);
    let cw = ui.ctx().fonts_mut(|f| f.glyph_width(&font, 'm'));
    (0..view.columns.len())
        .map(|c| {
            let longest = view
                .grid
                .rows
                .iter()
                .take(200)
                .map(|r| {
                    r.get(c)
                        .and_then(|v| v.as_deref())
                        .map_or(4, |v| v.chars().count())
                })
                .chain(std::iter::once(view.columns[c].name.chars().count() + 3))
                .max()
                .unwrap_or(4);
            (longest.min(48) as f32 * cw + 22.0).max(64.0)
        })
        .collect()
}

/// What a table tab's toolbar and grid asked for this frame.
#[derive(Default)]
struct Asked {
    copy: Option<String>,
    export: Option<(String, &'static str)>,
}

/// A table tab: its toolbar, the grid, and the line under it. True when the app has to
/// run something for it.
pub(super) fn show(ui: &mut Ui, view: &mut TableView) -> bool {
    let mut asked = Asked::default();
    if !view.local {
        toolbar(ui, view, &mut asked);
    }
    status_line(ui, view);
    grid(ui, view, &mut asked);
    big_editor(ui, view);
    if let Some(text) = asked.copy {
        ui.ctx().copy_text(text);
    }
    if let Some((text, ext)) = asked.export {
        let name = if view.table.is_empty() {
            "result"
        } else {
            &view.table
        };
        let file = rfd::FileDialog::new()
            .set_file_name(format!("{name}.{ext}"))
            .save_file();
        if let Some(path) = file {
            match std::fs::write(&path, text) {
                Ok(()) => view.notice = Some(format!("Saved {}", path.display())),
                Err(e) => view.error = Some(e.to_string()),
            }
        }
    }
    !view.wants.is_empty()
}

fn toolbar(ui: &mut Ui, view: &mut TableView, asked: &mut Asked) {
    let changes = view.changes();
    Frame::NONE
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(
                    RichText::new(format!("{}  {}", icons::DATABASE, view.table))
                        .strong()
                        .color(theme::STRONG()),
                );
                ui.add_space(8.0);
                if icon_button(ui, icons::REFRESH, "Read the rows again") {
                    if changes > 0 {
                        view.notice = Some("Save or discard your changes first".into());
                    } else {
                        view.reload(true);
                    }
                }
                if icon_button(ui, icons::ADD, "Add a row") {
                    view.add_row(None);
                }
                let can_delete = !view.selected.is_empty();
                if ui
                    .add_enabled(
                        can_delete,
                        egui::Button::new(RichText::new(icons::TRASH.to_string())).frame(false),
                    )
                    .on_hover_text("Delete the selected rows (Delete)")
                    .clicked()
                {
                    let rows = view.selected.clone();
                    view.delete_rows(&rows);
                }
                ui.separator();
                let save = egui::Button::new(
                    RichText::new(if changes > 0 {
                        format!(
                            "{}  Save {changes} change{}",
                            icons::SAVE,
                            if changes == 1 { "" } else { "s" }
                        )
                    } else {
                        format!("{}  Save", icons::SAVE)
                    })
                    .color(if changes > 0 {
                        Color32::WHITE
                    } else {
                        theme::COMMENT()
                    }),
                )
                .fill(if changes > 0 {
                    theme::ACCENT().gamma_multiply(0.85)
                } else {
                    theme::tint(8)
                });
                if ui
                    .add_enabled(changes > 0 && !view.saving, save)
                    .on_hover_text("All the changes, as one transaction (Ctrl+S)")
                    .clicked()
                {
                    view.request_save();
                }
                if changes > 0 && ui.button("Discard").clicked() {
                    view.discard();
                }
                if view.saving {
                    ui.spinner();
                }
                ui.separator();
                ui.menu_button(format!("{}  Export", icons::DOWNLOAD), |ui| {
                    export_menu(ui, view, asked);
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    pager(ui, view, changes);
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let half = ((ui.available_width() - 90.0) / 2.0).max(120.0);
                let search = ui.add(
                    TextEdit::singleline(&mut view.search)
                        .desired_width(half)
                        .hint_text(format!("{}  Search every column", icons::SEARCH)),
                );
                let filter = ui.add(
                    TextEdit::singleline(&mut view.filter)
                        .desired_width(half)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("WHERE  e.g. status = 'active' AND id > 100"),
                );
                let enter = ui.input(|i| i.key_pressed(Key::Enter));
                let go = ui.button("Apply").clicked()
                    || enter && (search.lost_focus() || filter.lost_focus());
                if go {
                    if changes > 0 {
                        view.notice = Some("Save or discard your changes first".into());
                    } else {
                        view.applied_search = view.search.clone();
                        view.applied_filter = view.filter.clone();
                        view.page = 0;
                        view.reload(true);
                    }
                }
            });
        });
}

fn pager(ui: &mut Ui, view: &mut TableView, changes: usize) {
    let first = view.page * view.page_size;
    let shown = view.grid.rows.len();
    let total = view.total;
    let last_page = total.map(|t| (t as usize).saturating_sub(1) / view.page_size.max(1));
    let mut go = None;
    if ui
        .add_enabled(
            last_page.is_none_or(|l| view.page < l) && shown == view.page_size,
            egui::Button::new(icons::CHEVRON_RIGHT.to_string()).frame(false),
        )
        .on_hover_text("Next page")
        .clicked()
    {
        go = Some(view.page + 1);
    }
    let range = if shown == 0 {
        "No rows".to_string()
    } else {
        format!("{}–{}", first + 1, first + shown)
    };
    let of = total
        .map(|t| format!(" of {}", group(t)))
        .unwrap_or_default();
    ui.label(
        RichText::new(format!("{range}{of}"))
            .small()
            .color(theme::COMMENT()),
    );
    if ui
        .add_enabled(
            view.page > 0,
            egui::Button::new(icons::ARROW_LEFT.to_string()).frame(false),
        )
        .on_hover_text("Previous page")
        .clicked()
    {
        go = Some(view.page - 1);
    }
    let mut size = view.page_size;
    egui::ComboBox::from_id_salt(("page_size", &view.table))
        .selected_text(format!("{size} rows"))
        .width(86.0)
        .show_ui(ui, |ui| {
            for choice in [50, 100, 200, 500, 1000] {
                ui.selectable_value(&mut size, choice, format!("{choice} rows"));
            }
        });
    if size != view.page_size {
        if changes > 0 {
            view.notice = Some("Save or discard your changes first".into());
        } else {
            view.page_size = size;
            view.page = 0;
            view.reload(false);
        }
    }
    if let Some(page) = go {
        if changes > 0 {
            view.notice = Some("Save or discard your changes first".into());
        } else {
            view.page = page;
            view.reload(false);
        }
    }
}

/// A number with its thousands apart: 12 345.
fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

fn export_menu(ui: &mut Ui, view: &TableView, asked: &mut Asked) {
    let all = view.rows_for_copy(true);
    let what = if view.local {
        "the result"
    } else {
        "this page"
    };
    if menu_item(ui, icons::COPY, &format!("Copy {what} as CSV")) {
        asked.copy = Some(view.text_of(&all, true, ','));
    }
    if menu_item(ui, icons::COPY, &format!("Copy {what} as JSON")) {
        asked.copy = Some(view.json_of(&all));
    }
    if menu_item(ui, icons::COPY, &format!("Copy {what} as SQL INSERTs")) {
        asked.copy = Some(view.inserts_of(&all));
    }
    if menu_item(
        ui,
        icons::COPY,
        &format!("Copy {what} as tab-separated text"),
    ) {
        asked.copy = Some(view.text_of(&all, true, '\t'));
    }
    ui.separator();
    if menu_item(ui, icons::SAVE, "Save as CSV…") {
        asked.export = Some((view.text_of(&all, true, ','), "csv"));
    }
    if menu_item(ui, icons::SAVE, "Save as JSON…") {
        asked.export = Some((view.json_of(&all), "json"));
    }
    if menu_item(ui, icons::SAVE, "Save as SQL…") {
        asked.export = Some((view.inserts_of(&all), "sql"));
    }
}

fn status_line(ui: &mut Ui, view: &mut TableView) {
    let mut parts: Vec<(String, Color32)> = Vec::new();
    if view.loading {
        parts.push(("Loading…".into(), theme::COMMENT()));
    }
    if let Some(error) = &view.error {
        parts.push((error.clone(), theme::ANSI()[1]));
    } else if let Some(notice) = &view.notice {
        parts.push((notice.clone(), theme::COMMENT()));
    }
    if !view.selected.is_empty() {
        let n = view.selected.len();
        parts.push((
            format!("{n} row{} selected", if n == 1 { "" } else { "s" }),
            theme::COMMENT(),
        ));
    }
    if view.local {
        let n = view.grid.rows.len();
        parts.push((
            format!(
                "{n}{} row{}",
                if view.grid.cut { "+" } else { "" },
                if n == 1 { "" } else { "s" }
            ),
            theme::COMMENT(),
        ));
    }
    if parts.is_empty() {
        return;
    }
    Frame::NONE
        .inner_margin(Margin::symmetric(12, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if view.loading {
                    ui.spinner();
                }
                for (text, color) in parts {
                    ui.add(egui::Label::new(RichText::new(text).small().color(color)).truncate());
                    ui.add_space(8.0);
                }
            });
        });
}

/// The rows: a header that stays on top, the row numbers at the left, and the cells.
fn grid(ui: &mut Ui, view: &mut TableView, asked: &mut Asked) {
    if view.columns.is_empty() {
        return;
    }
    if view.widths.len() != view.columns.len() {
        view.widths = measure(ui, view);
    }
    let font = FontId::monospace(12.5);
    let row_h = (ui.ctx().fonts_mut(|f| f.row_height(&font)) + 9.0).round();
    let total: f32 = view.widths.iter().sum::<f32>() + NUM_W;
    let grid_id = ui.id().with("table_grid");
    let area = ui.available_rect_before_wrap();
    // The whole grid can have the keyboard: arrows, Enter, Delete, Ctrl+A, Ctrl+C.
    let back = ui.interact(area, grid_id, Sense::click());
    if back.clicked() {
        back.request_focus();
    }
    let focused = ui.memory(|m| m.has_focus(grid_id));
    if focused {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                grid_id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }
    if focused && view.editing.is_none() && view.big_edit.is_none() {
        keys(ui, view, asked);
    }

    let mut menu: Option<(usize, usize)> = None;
    let mut clicked: Option<(usize, Option<usize>, Modifiers, bool)> = None;
    let mut sort_by = None;
    let mut resize: Option<(usize, f32)> = None;
    let mut commit = false;
    let mut cancel = false;
    let mut tab_on = false;

    ScrollArea::horizontal()
        .id_salt("table_h")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.set_min_width(total);
            // ---- the header ----
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let (rect, _) = ui.allocate_exact_size(vec2(NUM_W, row_h + 4.0), Sense::hover());
                ui.painter().rect_filled(rect, 0.0, theme::tint(10));
                for (c, column) in view.columns.iter().enumerate() {
                    let width = view.widths[c];
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(width, row_h + 4.0), Sense::click());
                    let hovered = resp.hovered();
                    ui.painter().rect_filled(
                        rect,
                        0.0,
                        if hovered {
                            theme::tint(16)
                        } else {
                            theme::tint(10)
                        },
                    );
                    ui.painter()
                        .vline(rect.right() - 0.5, rect.y_range(), (1.0, theme::LINE()));
                    let mut text = String::new();
                    if column.key {
                        text.push_str(&format!("{}  ", icons::KEY));
                    }
                    text.push_str(&column.name);
                    let arrow = match view.sort {
                        Some((s, true)) if s == c => " ▲",
                        Some((s, false)) if s == c => " ▼",
                        _ => "",
                    };
                    let galley = egui::WidgetText::from(
                        RichText::new(format!("{text}{arrow}"))
                            .font(font.clone())
                            .strong()
                            .color(theme::STRONG()),
                    )
                    .into_galley(
                        ui,
                        Some(TextWrapMode::Truncate),
                        width - 14.0,
                        egui::TextStyle::Monospace,
                    );
                    ui.painter().galley(
                        rect.left_center() + vec2(7.0, -galley.size().y / 2.0),
                        galley,
                        theme::STRONG(),
                    );
                    let mut hint = column.kind.clone();
                    if column.key {
                        hint.push_str("  · primary key");
                    }
                    if !column.nullable {
                        hint.push_str("  · not null");
                    }
                    if column.auto {
                        hint.push_str("  · filled in by the database");
                    }
                    if let Some(default) = &column.default {
                        hint.push_str(&format!("  · default {default}"));
                    }
                    hint.push_str("\nClick to sort; drag its right edge to resize");
                    if resp
                        .on_hover_text(hint.trim_start_matches("  · "))
                        .clicked()
                    {
                        sort_by = Some(c);
                    }
                    // The right edge of a heading is a handle for the column's width.
                    let handle = egui::Rect::from_min_max(
                        rect.right_top() - vec2(4.0, 0.0),
                        rect.right_bottom() + vec2(3.0, 0.0),
                    );
                    let drag = ui.interact(handle, grid_id.with(("width", c)), Sense::drag());
                    if drag.hovered() || drag.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    if drag.dragged() {
                        resize = Some((c, drag.drag_delta().x));
                    }
                }
            });

            // ---- the rows ----
            // All the height there is, whatever the scroll area around it would give.
            let height = ui.available_height().max(row_h * 3.0);
            let mut area = ScrollArea::vertical()
                .id_salt("table_v")
                .auto_shrink(false)
                .min_scrolled_height(height)
                .max_height(height);
            if let Some(row) = view.scroll_to.take() {
                if let Some(at) = view.order.iter().position(|&r| r == row) {
                    let offset = ui.ctx().memory(|m| {
                        m.data
                            .get_temp::<f32>(grid_id.with("offset"))
                            .unwrap_or(0.0)
                    });
                    let height = ui.available_height() - row_h;
                    let y = at as f32 * row_h;
                    if y < offset || y > offset + height - row_h {
                        area = area.vertical_scroll_offset((y - height / 2.0).max(0.0));
                    }
                }
            }
            // Rows touch: the space between them counts when working out which are in view.
            ui.spacing_mut().item_spacing.y = 0.0;
            let shown = area.show_rows(ui, row_h, view.order.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for at in range {
                    let row = view.order[at];
                    let deleted = view.deleted.contains(&row);
                    let added = row >= view.base();
                    let selected = view.selected.contains(&row);
                    let stripe = if at % 2 == 1 {
                        theme::tint(4)
                    } else {
                        Color32::TRANSPARENT
                    };
                    let row_fill = if selected {
                        theme::SELECTION()
                    } else if deleted {
                        theme::ANSI()[1].gamma_multiply(0.16)
                    } else if added {
                        theme::ANSI()[2].gamma_multiply(0.13)
                    } else {
                        stripe
                    };
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        // The row's number: click to select it.
                        let (rect, resp) =
                            ui.allocate_exact_size(vec2(NUM_W, row_h), Sense::click());
                        ui.painter().rect_filled(rect, 0.0, theme::tint(7));
                        let label = if added {
                            "+".to_string()
                        } else {
                            (view.page * view.page_size + row + 1).to_string()
                        };
                        ui.painter().text(
                            rect.right_center() - vec2(8.0, 0.0),
                            Align2::RIGHT_CENTER,
                            label,
                            font.clone(),
                            if selected {
                                theme::STRONG()
                            } else {
                                theme::COMMENT()
                            },
                        );
                        if resp.clicked() {
                            let mods = ui.input(|i| i.modifiers);
                            clicked = Some((row, None, mods, false));
                        }
                        if resp.secondary_clicked() {
                            if !view.selected.contains(&row) {
                                clicked = Some((row, None, Modifiers::NONE, false));
                            }
                        }
                        resp.context_menu(|ui| {
                            menu = Some((row, usize::MAX));
                            row_menu(ui, view, row, None, asked);
                        });
                        for c in 0..view.columns.len() {
                            let width = view.widths[c];
                            let (rect, resp) =
                                ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                            let edited = row < view.base() && view.edits.contains_key(&(row, c));
                            let fill = if edited && !deleted {
                                theme::ANSI()[3].gamma_multiply(0.18)
                            } else {
                                row_fill
                            };
                            ui.painter().rect_filled(rect, 0.0, fill);
                            ui.painter().vline(
                                rect.right() - 0.5,
                                rect.y_range(),
                                (1.0, theme::tint(6)),
                            );
                            let is_cell = view.cell == Some((row, c));
                            if let Some((er, ec, text)) = &mut view.editing {
                                if *er == row && *ec == c {
                                    let edit = ui.put(
                                        rect.shrink2(vec2(2.0, 1.0)),
                                        TextEdit::singleline(text)
                                            .font(font.clone())
                                            .margin(Margin::symmetric(4, 2)),
                                    );
                                    if view.focus_editor {
                                        view.focus_editor = false;
                                        edit.request_focus();
                                    }
                                    let (enter, escape, tab) = ui.input(|i| {
                                        (
                                            i.key_pressed(Key::Enter),
                                            i.key_pressed(Key::Escape),
                                            i.key_pressed(Key::Tab),
                                        )
                                    });
                                    if escape {
                                        cancel = true;
                                    } else if edit.lost_focus() {
                                        commit = true;
                                        tab_on = tab && !enter;
                                    }
                                    continue;
                                }
                            }
                            let value: Option<String> = view.value(row, c).map(str::to_string);
                            let text = if view.is_default(row, c) {
                                RichText::new("DEFAULT").italics().color(theme::COMMENT())
                            } else {
                                match value.as_deref() {
                                    None => RichText::new("NULL").italics().color(theme::COMMENT()),
                                    Some(v) => {
                                        let shown: String = v.chars().take(300).collect();
                                        let text = RichText::new(shown.replace('\n', "⏎"))
                                            .color(theme::TEXT());
                                        if deleted {
                                            text.strikethrough()
                                        } else {
                                            text
                                        }
                                    }
                                }
                            }
                            .font(font.clone());
                            let galley = egui::WidgetText::from(text).into_galley(
                                ui,
                                Some(TextWrapMode::Truncate),
                                width - 14.0,
                                egui::TextStyle::Monospace,
                            );
                            ui.painter().galley(
                                rect.left_center() + vec2(7.0, -galley.size().y / 2.0),
                                galley,
                                theme::TEXT(),
                            );
                            if is_cell {
                                ui.painter().rect_stroke(
                                    rect.shrink(1.0),
                                    2.0,
                                    (1.5, theme::ACCENT()),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if resp.double_clicked() {
                                clicked = Some((row, Some(c), Modifiers::NONE, true));
                            } else if resp.clicked() {
                                let mods = ui.input(|i| i.modifiers);
                                clicked = Some((row, Some(c), mods, false));
                            }
                            if resp.secondary_clicked() {
                                view.cell = Some((row, c));
                                if !view.selected.contains(&row) {
                                    view.selected.clear();
                                }
                            }
                            let resp = match value.as_deref() {
                                Some(v) if v.chars().count() > 40 || v.contains('\n') => {
                                    let hint: String = v.chars().take(2000).collect();
                                    resp.on_hover_text(hint)
                                }
                                _ => resp,
                            };
                            resp.context_menu(|ui| {
                                menu = Some((row, c));
                                row_menu(ui, view, row, Some(c), asked);
                            });
                        }
                    });
                }
            });
            ui.ctx().memory_mut(|m| {
                m.data
                    .insert_temp(grid_id.with("offset"), shown.state.offset.y)
            });
        });

    if cancel {
        view.editing = None;
        back.request_focus();
    } else if commit {
        let at = view.editing.as_ref().map(|(r, c, _)| (*r, *c));
        view.commit_edit();
        back.request_focus();
        // Tab goes on to the next cell, and edits it.
        if let (true, Some((row, col))) = (tab_on, at) {
            if col + 1 < view.columns.len() {
                view.start_edit(row, col + 1);
            }
        }
    }
    if let Some((c, delta)) = resize {
        view.widths[c] = (view.widths[c] + delta).max(40.0);
    }
    if let Some(c) = sort_by {
        let next = match view.sort {
            Some((s, true)) if s == c => Some((c, false)),
            Some((s, false)) if s == c => None,
            _ => Some((c, true)),
        };
        if view.local {
            view.sort = next;
            view.reorder();
        } else if view.changes() > 0 {
            view.notice = Some("Save or discard your changes first".into());
        } else {
            view.sort = next;
            view.page = 0;
            view.reload(false);
        }
    }
    if let Some((row, col, mods, double)) = clicked {
        back.request_focus();
        if view.editing.is_some() {
            view.commit_edit();
        }
        match col {
            Some(col) if double => view.start_edit(row, col),
            Some(col) => {
                view.cell = Some((row, col));
                if mods.command || mods.shift {
                    select(view, row, mods);
                } else {
                    view.selected.clear();
                    view.anchor = Some(row);
                }
            }
            None => {
                select(view, row, mods);
                view.cell = None;
            }
        }
    }
    let _ = menu;
}

/// A click on a row: alone, added to the others (Ctrl), or all the rows from the last one
/// (Shift).
fn select(view: &mut TableView, row: usize, mods: Modifiers) {
    if mods.shift {
        let from = view.anchor.unwrap_or(row);
        let a = view.order.iter().position(|&r| r == from).unwrap_or(0);
        let b = view.order.iter().position(|&r| r == row).unwrap_or(0);
        let (a, b) = (a.min(b), a.max(b));
        if !mods.command {
            view.selected.clear();
        }
        view.selected.extend(view.order[a..=b].iter().copied());
    } else if mods.command {
        if !view.selected.remove(&row) {
            view.selected.insert(row);
        }
        view.anchor = Some(row);
    } else {
        view.selected = BTreeSet::from([row]);
        view.anchor = Some(row);
    }
}

/// The keys of a grid that has the keyboard.
fn keys(ui: &mut Ui, view: &mut TableView, asked: &mut Asked) {
    let copy = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
    if copy {
        asked.copy = Some(match (view.selected.is_empty(), view.cell) {
            (true, Some((row, col))) => view.value(row, col).unwrap_or("NULL").to_string(),
            _ => {
                let rows = view.rows_for_copy(false);
                view.text_of(&rows, false, '\t')
            }
        });
        view.notice = Some("Copied".into());
    }
    let pressed =
        |ui: &mut Ui, mods: Modifiers, key: Key| ui.input_mut(|i| i.consume_key(mods, key));
    if pressed(ui, Modifiers::COMMAND, Key::A) {
        view.selected = view.order.iter().copied().collect();
    }
    if pressed(ui, Modifiers::NONE, Key::Delete) && !view.local {
        let rows = if view.selected.is_empty() {
            view.cell
                .map(|(r, _)| BTreeSet::from([r]))
                .unwrap_or_default()
        } else {
            view.selected.clone()
        };
        view.delete_rows(&rows);
    }
    if pressed(ui, Modifiers::NONE, Key::Escape) {
        view.selected.clear();
    }
    if let Some((row, col)) = view.cell {
        if pressed(ui, Modifiers::NONE, Key::Enter) || pressed(ui, Modifiers::NONE, Key::F2) {
            view.start_edit(row, col);
            return;
        }
    }
    for (key, rows, cols) in [
        (Key::ArrowUp, -1, 0),
        (Key::ArrowDown, 1, 0),
        (Key::ArrowLeft, 0, -1),
        (Key::ArrowRight, 0, 1),
        (Key::PageUp, -20, 0),
        (Key::PageDown, 20, 0),
    ] {
        if pressed(ui, Modifiers::NONE, key) {
            view.step(rows, cols);
        }
        if pressed(ui, Modifiers::SHIFT, key) {
            let from = view.cell.map(|(r, _)| r);
            view.step(rows, cols);
            if let (Some(from), Some((to, _))) = (from, view.cell) {
                view.anchor.get_or_insert(from);
                select(view, to, Modifiers::SHIFT);
            }
        }
    }
    if pressed(ui, Modifiers::NONE, Key::Tab) {
        view.step(0, 1);
    }
}

/// The right-click menu of a row, or of one of its cells.
fn row_menu(ui: &mut Ui, view: &mut TableView, row: usize, col: Option<usize>, asked: &mut Asked) {
    let rows: BTreeSet<usize> = if view.selected.contains(&row) {
        view.selected.clone()
    } else {
        BTreeSet::from([row])
    };
    let list: Vec<usize> = view
        .order
        .iter()
        .copied()
        .filter(|r| rows.contains(r))
        .collect();
    let many = rows.len() > 1;
    let editable = !view.local && !view.deleted.contains(&row);
    if let Some(col) = col {
        if editable && menu_item(ui, icons::EDIT, "Edit    Enter") {
            view.start_edit(row, col);
        }
        if editable && menu_item(ui, icons::EDIT, "Edit in a larger box…") {
            let value = view.value(row, col).unwrap_or("").to_string();
            view.big_edit = Some((row, col, value));
        }
        if editable && view.columns[col].nullable && menu_item(ui, icons::CLEAR, "Set to NULL") {
            view.set(row, col, None);
        }
        if menu_item(ui, icons::COPY, "Copy value") {
            asked.copy = Some(view.value(row, col).unwrap_or("NULL").to_string());
        }
        if !view.local && menu_item(ui, icons::SEARCH, "Filter by this value") {
            let name = ident(view.engine, &view.columns[col].name);
            view.filter = match view.value(row, col) {
                None => format!("{name} IS NULL"),
                Some(v) => format!("{name} = {}", literal(view.engine, Some(v))),
            };
            if view.changes() == 0 {
                view.applied_filter = view.filter.clone();
                view.page = 0;
                view.reload(true);
            }
        }
        ui.separator();
    }
    let rows_word = if many { "rows" } else { "row" };
    if menu_item(
        ui,
        icons::COPY,
        &format!("Copy {rows_word} as tab-separated text"),
    ) {
        asked.copy = Some(view.text_of(&list, false, '\t'));
    }
    if menu_item(ui, icons::COPY, &format!("Copy {rows_word} as JSON")) {
        asked.copy = Some(view.json_of(&list));
    }
    if menu_item(ui, icons::COPY, &format!("Copy {rows_word} as SQL INSERT")) {
        asked.copy = Some(view.inserts_of(&list));
    }
    if view.local {
        return;
    }
    ui.separator();
    if !many && menu_item(ui, icons::ADD, "Duplicate row") {
        view.add_row(Some(row));
    }
    let changed = rows
        .iter()
        .any(|r| view.deleted.contains(r) || view.edits.keys().any(|(er, _)| er == r));
    if changed && menu_item(ui, icons::DISCARD, "Undo the changes") {
        view.revert_rows(&rows);
    }
    if menu_item(ui, icons::TRASH, &format!("Delete {rows_word}    Delete")) {
        view.delete_rows(&rows);
    }
}

/// A long value, edited in a box of its own.
fn big_editor(ui: &mut Ui, view: &mut TableView) {
    let Some((_, col, text)) = &mut view.big_edit else {
        return;
    };
    let name = view
        .columns
        .get(*col)
        .map_or("", |c| c.name.as_str())
        .to_string();
    let answer = chrome::modal(
        ui.ctx(),
        "table_value",
        &format!("Edit {name}"),
        "Set",
        chrome::Tone::Normal,
        true,
        620.0,
        |ui| {
            ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                chrome::field(ui, "table_value", text, "", 14);
            });
            false
        },
    );
    match answer {
        chrome::Answer::Confirm => {
            let (row, col, text) = view.big_edit.take().unwrap();
            view.set(row, col, Some(text));
        }
        chrome::Answer::Cancel => view.big_edit = None,
        chrome::Answer::Waiting => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn users(engine: Engine) -> TableView {
        let mut view = TableView::new(0, engine, "users".into());
        view.wants.clear();
        view.set_columns(vec![
            Column {
                name: "id".into(),
                key: true,
                auto: true,
                ..Column::default()
            },
            Column {
                name: "name".into(),
                nullable: true,
                ..Column::default()
            },
            Column {
                name: "note".into(),
                nullable: true,
                ..Column::default()
            },
        ]);
        view.set_page(Grid {
            columns: vec!["id".into(), "name".into(), "note".into()],
            rows: vec![
                vec![Some("1".into()), Some("Ana".into()), None],
                vec![Some("2".into()), Some("O'Neil".into()), Some("a\\b".into())],
            ],
            cut: false,
        });
        view
    }

    #[test]
    fn values_are_quoted_for_each_engine() {
        assert_eq!(literal(Engine::MySql, Some("O'Ne\\il")), "'O''Ne\\\\il'");
        assert_eq!(literal(Engine::Postgres, Some("O'Ne\\il")), "'O''Ne\\il'");
        assert_eq!(literal(Engine::Postgres, None), "NULL");
        assert_eq!(ident(Engine::MySql, "we`ird"), "`we``ird`");
        assert_eq!(ident(Engine::Postgres, "we\"ird"), "\"we\"\"ird\"");
    }

    #[test]
    fn pages_are_read_sorted_and_filtered() {
        let mut view = users(Engine::Postgres);
        view.table = "public.users".into();
        view.sort = Some((1, false));
        view.applied_filter = "id > 1".into();
        view.applied_search = "an".into();
        view.page = 2;
        view.page_size = 50;
        assert_eq!(
            view.page_sql(),
            "SELECT * FROM \"public\".\"users\" WHERE (id > 1) AND (CAST(\"id\" AS TEXT) ILIKE '%an%' OR CAST(\"name\" AS TEXT) ILIKE '%an%' OR CAST(\"note\" AS TEXT) ILIKE '%an%') ORDER BY \"name\" DESC LIMIT 50 OFFSET 100;"
        );
        assert!(view
            .count_sql()
            .starts_with("SELECT COUNT(*) FROM \"public\".\"users\" WHERE (id > 1)"));
    }

    #[test]
    fn changes_are_saved_by_primary_key_in_one_transaction() {
        let mut view = users(Engine::MySql);
        view.set(1, 1, Some("Bob".into()));
        view.set(1, 2, None);
        // Setting a cell back to what it was is no change.
        view.set(0, 1, Some("X".into()));
        view.set(0, 1, Some("Ana".into()));
        view.add_row(Some(0));
        view.delete_rows(&BTreeSet::from([0]));
        assert_eq!(view.changes(), 3);
        let sql = view.save_sql().unwrap();
        assert_eq!(
            sql,
            "START TRANSACTION;\nDELETE FROM `users` WHERE `id` = '1';\nUPDATE `users` SET `name` = 'Bob', `note` = NULL WHERE `id` = '2';\nINSERT INTO `users` (`name`, `note`) VALUES ('Ana', NULL);\nCOMMIT;"
        );
    }

    #[test]
    fn rows_without_a_key_are_found_by_their_values() {
        let mut view = users(Engine::MySql);
        for column in &mut view.columns {
            column.key = false;
        }
        view.set(1, 1, Some("Bob".into()));
        assert_eq!(
            view.save_statements(),
            ["UPDATE `users` SET `name` = 'Bob' WHERE `id` = '2' AND `name` = 'O''Neil' AND `note` = 'a\\\\b' LIMIT 1;"]
        );
        let mut view = users(Engine::Postgres);
        view.add_row(None);
        assert_eq!(
            view.save_statements(),
            ["INSERT INTO \"users\" DEFAULT VALUES;"]
        );
    }

    #[test]
    fn rows_are_copied_as_csv_json_and_sql() {
        let view = users(Engine::Postgres);
        let rows = view.rows_for_copy(true);
        assert_eq!(
            view.text_of(&rows, true, ','),
            "id,name,note\n1,Ana,\n2,O'Neil,a\\b\n"
        );
        assert!(view.json_of(&rows[..1]).contains("\"note\": null"));
        assert_eq!(
            view.inserts_of(&rows[1..]),
            "INSERT INTO \"users\" (\"id\", \"name\", \"note\") VALUES ('2', 'O''Neil', 'a\\b');\n"
        );
    }

    #[test]
    fn results_sort_here_as_numbers_or_text() {
        let mut view = TableView::result(Grid {
            columns: vec!["n".into()],
            rows: vec![vec![Some("10".into())], vec![Some("9".into())], vec![None]],
            cut: false,
        });
        view.sort = Some((0, true));
        view.reorder();
        assert_eq!(view.order, [2, 1, 0]);
        assert_eq!(group(1234567), "1 234 567");
    }
}
