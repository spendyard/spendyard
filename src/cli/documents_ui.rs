//! `spendyard documents ui`: the store's documents in the terminal. The list
//! on the left, as `documents list` shows it; the one picked on the right,
//! whole, as `documents get` prints it; and the corrections `documents edit`
//! makes, a key each.
//!
//! Keys: ↑↓ (or j k) move, PgUp PgDn, g G the ends; / searches as you type;
//! a shows everything stored or only receipts and invoices; p and i mark it
//! personal or ignored, and again to undo; v names its vendor, n notes it;
//! o opens the original; J K scroll the right side; q quits.

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as TableRow, Table, TableState, Wrap};
use ratatui::{DefaultTerminal, Frame};
use spendyard_core::spend::Row;

use crate::cli::documents::describe;
use crate::cli::payments;
use crate::cli::reports::{EditOptions, apply_edit, listed_by_default, rows};
use crate::cli::store::Store;

/// Runs it until q. Works out the payments first, as `documents get` does, so the numbers it shows are those
/// `spendyard payments` lists.
pub async fn run() -> Result<()> {
    let store = Store::open()?;

    eprintln!("spendyard: working out payments…");

    payments::regroup(&store).await?;

    let mut app = App::load(store)?;

    let mut terminal = ratatui::init();
    let outcome = app.run(&mut terminal);

    ratatui::restore();

    outcome
}

/// What the line at the bottom is for.
enum Mode {
    /// Moving about; the keys are listed.
    Browse,

    /// Typing a search: the list follows each key.
    Search,

    /// Typing a correction, for the document picked.
    Prompt { field: Field, text: String },
}

#[derive(Clone, Copy)]
enum Field {
    Vendor,
    Note,
}

impl Field {
    fn label(self) -> &'static str {
        match self {
            Field::Vendor => "Vendor",
            Field::Note => "Note",
        }
    }
}

struct App {
    store: Store,

    /// Every row in the store, newest first.
    rows: Vec<Row>,

    /// Those shown, as indexes into `rows`.
    shown: Vec<usize>,

    table: TableState,

    /// Everything stored, or only receipts, invoices and the like.
    all: bool,

    query: String,
    mode: Mode,

    /// The picked document, as `documents get` prints it.
    detail: Vec<String>,
    detail_scroll: u16,

    /// What just happened, or went wrong: shown at the bottom until the next key.
    message: Option<String>,

    quit: bool,
}

impl App {
    fn load(store: Store) -> Result<App> {
        let mut app = App {
            store,
            rows: Vec::new(),
            shown: Vec::new(),
            table: TableState::default(),
            all: false,
            query: String::new(),
            mode: Mode::Browse,
            detail: Vec::new(),
            detail_scroll: 0,
            message: None,
            quit: false,
        };

        app.reload()?;

        Ok(app)
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|frame| self.draw(frame))?;

            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.message = None;

                self.on_key(key);
            }
        }

        Ok(())
    }

    // --- what's shown ---------------------------------------------------------------------------

    /// The store's rows again, after a correction: the same document stays picked.
    fn reload(&mut self) -> Result<()> {
        self.rows = rows(&self.store)?;

        self.refilter();

        Ok(())
    }

    /// The rows the toggles and the search let through, keeping the picked one where it can.
    fn refilter(&mut self) {
        let picked = self.picked().map(|row| row.id);
        let query = self.query.to_lowercase();

        self.shown = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| self.all || listed_by_default(row))
            .filter(|(_, row)| query.is_empty() || matches_query(row, &query))
            .map(|(index, _)| index)
            .collect();

        let position = picked
            .and_then(|id| {
                self.shown
                    .iter()
                    .position(|&index| self.rows[index].id == id)
            })
            .or(if self.shown.is_empty() { None } else { Some(0) });

        self.table.select(position);

        self.load_detail();
    }

    fn picked(&self) -> Option<&Row> {
        let position = self.table.selected()?;
        let index = *self.shown.get(position)?;

        self.rows.get(index)
    }

    fn load_detail(&mut self) {
        self.detail_scroll = 0;

        let Some(id) = self.picked().map(|row| row.id) else {
            self.detail = vec!["Nothing here: / searches, a shows everything stored".to_string()];

            return;
        };

        self.detail = describe(&self.store, id).unwrap_or_else(|error| vec![format!("{error:#}")]);
    }

    fn move_by(&mut self, step: isize) {
        if self.shown.is_empty() {
            return;
        }

        let last = self.shown.len() as isize - 1;
        let now = self.table.selected().unwrap_or(0) as isize;
        let next = (now + step).clamp(0, last);

        self.table.select(Some(next as usize));

        self.load_detail();
    }

    // --- keys -----------------------------------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent) {
        match &mut self.mode {
            Mode::Browse => self.on_browse_key(key),

            Mode::Search => self.on_search_key(key),

            Mode::Prompt { field, text } => {
                let field = *field;

                match key.code {
                    KeyCode::Enter => {
                        let typed = text.trim().to_string();

                        self.mode = Mode::Browse;

                        self.correct(field, typed);
                    }

                    KeyCode::Esc => self.mode = Mode::Browse,

                    KeyCode::Backspace => {
                        text.pop();
                    }

                    KeyCode::Char(c) => text.push(c),

                    _ => {}
                }
            }
        }
    }

    fn on_browse_key(&mut self, key: KeyEvent) {
        let page = 10;

        match key.code {
            KeyCode::Char('q') => self.quit = true,

            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,

            // A search is cleared first; then it's quitting.
            KeyCode::Esc if !self.query.is_empty() => {
                self.query.clear();

                self.refilter();
            }

            KeyCode::Esc => self.quit = true,

            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::PageDown => self.move_by(page),
            KeyCode::PageUp => self.move_by(-page),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_by(isize::MAX / 2),

            KeyCode::Char('J') => self.detail_scroll = self.detail_scroll.saturating_add(3),
            KeyCode::Char('K') => self.detail_scroll = self.detail_scroll.saturating_sub(3),

            KeyCode::Char('/') => self.mode = Mode::Search,

            KeyCode::Char('a') => {
                self.all = !self.all;

                self.refilter();
            }

            KeyCode::Char('p') => {
                self.toggle(|row| row.personal, |edit, on| edit.personal = Some(on))
            }
            KeyCode::Char('i') => self.toggle(|row| row.ignored, |edit, on| edit.ignore = Some(on)),

            KeyCode::Char('v') => self.prompt(Field::Vendor),
            KeyCode::Char('n') => self.prompt(Field::Note),

            KeyCode::Char('o') => self.open_original(),

            _ => {}
        }
    }

    fn on_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.mode = Mode::Browse,

            KeyCode::Esc => {
                self.query.clear();
                self.mode = Mode::Browse;

                self.refilter();
            }

            KeyCode::Backspace => {
                self.query.pop();

                self.refilter();
            }

            KeyCode::Down => self.move_by(1),
            KeyCode::Up => self.move_by(-1),

            KeyCode::Char(c) => {
                self.query.push(c);

                self.refilter();
            }

            _ => {}
        }
    }

    // --- corrections ----------------------------------------------------------------------------

    /// Marks the picked document, or unmarks it when it's marked already.
    fn toggle(&mut self, is_on: fn(&Row) -> bool, set: fn(&mut EditOptions, bool)) {
        let Some(row) = self.picked() else {
            return;
        };

        let mut edit = no_edit(row.id);

        set(&mut edit, !is_on(row));

        self.save(edit);
    }

    fn prompt(&mut self, field: Field) {
        let Some(row) = self.picked() else {
            return;
        };

        // The vendor's name to change; a note starts empty.
        let text = match field {
            Field::Vendor => row.vendor_name.clone(),
            Field::Note => String::new(),
        };

        self.mode = Mode::Prompt { field, text };
    }

    fn correct(&mut self, field: Field, typed: String) {
        let Some(row) = self.picked() else {
            return;
        };

        let mut edit = no_edit(row.id);

        match field {
            // No name is no change: a vendor is never nameless.
            Field::Vendor if typed.is_empty() => return,
            Field::Vendor => edit.vendor = Some(typed),
            Field::Note => edit.note = Some(typed),
        }

        self.save(edit);
    }

    fn save(&mut self, edit: EditOptions) {
        let id = edit.id;

        let saved = apply_edit(&self.store, &edit).and_then(|()| self.reload());

        self.message = Some(match saved {
            Ok(()) => format!("Document {id} corrected"),
            Err(error) => format!("Couldn't correct document {id}: {error:#}"),
        });
    }

    /// The original, in the app that opens its kind of file, when it was read from a file still there.
    fn open_original(&mut self) {
        let Some(row) = self.picked() else {
            return;
        };

        let path = row
            .source
            .as_deref()
            .and_then(|source| source.strip_prefix("file:"))
            .map(std::path::PathBuf::from)
            .filter(|path| path.exists());

        let Some(path) = path else {
            self.message = Some("Its original isn't a file here".to_string());

            return;
        };

        let opener = match std::env::consts::OS {
            "macos" => "open",
            "windows" => "explorer",
            _ => "xdg-open",
        };

        let opened = std::process::Command::new(opener)
            .arg(&path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        if let Err(error) = opened {
            self.message = Some(format!("Couldn't open {}: {error}", path.display()));
        }
    }

    // --- drawing --------------------------------------------------------------------------------

    fn draw(&mut self, frame: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let [list, detail] =
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                .areas(body);

        frame.render_widget(self.header(), header);

        self.draw_list(frame, list);
        self.draw_detail(frame, detail);

        frame.render_widget(self.footer(), footer);
    }

    fn header(&self) -> Paragraph<'_> {
        let which = if self.all {
            "everything stored"
        } else {
            "receipts and invoices"
        };

        let search = if self.query.is_empty() {
            String::new()
        } else {
            format!(", matching \"{}\"", self.query)
        };

        let title = format!(
            " Documents  {} of {}, {which}{search}",
            self.shown.len(),
            self.rows.len()
        );

        Paragraph::new(title).bold()
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let rows = self.shown.iter().map(|&index| list_row(&self.rows[index]));

        let widths = [
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Fill(1),
            Constraint::Length(16),
            Constraint::Length(3),
        ];

        let header = TableRow::new(["ID", "Date", "Type", "Vendor", "Total", ""]).dim();

        let table = Table::new(rows, widths)
            .header(header)
            .column_spacing(1)
            .block(Block::default().borders(Borders::RIGHT))
            .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        frame.render_stateful_widget(table, area, &mut self.table);
    }

    fn draw_detail(&self, frame: &mut Frame, area: Rect) {
        let lines: Vec<Line> = self
            .detail
            .iter()
            .map(|line| Line::raw(line.as_str()))
            .collect();

        let detail = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.detail_scroll, 0));

        let inset = Rect {
            x: area.x + 1,
            width: area.width.saturating_sub(1),
            ..area
        };

        frame.render_widget(detail, inset);
    }

    fn footer(&self) -> Paragraph<'_> {
        let text = match &self.mode {
            Mode::Search => format!(" /{}▏  Enter keeps it, Esc clears it", self.query),

            Mode::Prompt { field, text } => {
                format!(" {}: {text}▏  Enter saves, Esc cancels", field.label())
            }

            Mode::Browse => match &self.message {
                Some(message) => format!(" {message}"),

                None => " ↑↓ move  / search  a all  p personal  i ignore  v vendor  n note  o open  J K scroll  q quit"
                    .to_string(),
            },
        };

        Paragraph::new(text).reversed()
    }
}

/// An edit that changes nothing yet.
fn no_edit(id: i64) -> EditOptions {
    EditOptions {
        id,
        personal: None,
        ignore: None,
        vendor: None,
        note: None,
    }
}

/// Whether a row matches what was typed: its vendor, its number, its type or its ID.
fn matches_query(row: &Row, query: &str) -> bool {
    let fields = [
        Some(row.vendor_name.as_str()),
        Some(row.vendor_key.as_str()),
        Some(row.kind.as_str()),
        row.number.as_deref(),
    ];

    let in_fields = fields
        .into_iter()
        .flatten()
        .any(|field| field.to_lowercase().contains(query));

    in_fields || row.id.to_string() == query
}

/// A row of the list: P for personal, I for ignored, ! for flagged.
fn list_row(row: &Row) -> TableRow<'_> {
    let total = match (&row.total, &row.currency) {
        (Some(total), Some(currency)) => format!("{total} {currency}"),
        (Some(total), None) => total.clone(),
        _ => String::new(),
    };

    let marks: String = [
        (row.personal, 'P'),
        (row.ignored, 'I'),
        (row.flags > 0, '!'),
    ]
    .into_iter()
    .filter_map(|(on, mark)| on.then_some(mark))
    .collect();

    let cells = [
        Cell::from(row.id.to_string()),
        Cell::from(row.date.as_str()),
        Cell::from(row.kind.replace('_', " ")),
        Cell::from(row.vendor_name.as_str()),
        Cell::from(Line::from(total).right_aligned()),
        Cell::from(marks),
    ];

    let table_row = TableRow::new(cells);

    // Left out of spend: shown fainter.
    if row.personal || row.ignored {
        table_row.dim()
    } else {
        table_row
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::cli::store::Source;

    /// A store with one document in it, read as something that isn't about money: listed only with `a`.
    fn app_with_one_document() -> (App, i64) {
        let dir = std::env::temp_dir().join(format!(
            "spendyard-ui-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));

        let _ = std::fs::remove_dir_all(&dir);

        let mut store = Store::open_at(&dir.join("test.db")).unwrap();

        let bytes = std::fs::read(format!(
            "{}/crates/core/tests/fixtures/receipt.eml",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();

        let document = spendyard_core::read(&bytes, None).unwrap();

        let result = spendyard_core::result::not_about_money(
            spendyard_core::extract::schema::DocumentType::Other,
            0.9,
        );

        let source = Source {
            origin: "file:receipt.eml".to_string(),
            name: Some("receipt.eml"),
            received: None,
        };

        let id = store.save(&bytes, &document, &source, &result).unwrap();

        (App::load(store).unwrap(), id)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    /// The screen, as text.
    fn screen(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(140, 16)).unwrap();

        terminal.draw(|frame| app.draw(frame)).unwrap();

        terminal.backend().to_string()
    }

    #[test]
    fn shows_receipts_and_invoices_and_everything_with_a() {
        let (mut app, id) = app_with_one_document();

        let only_receipts = screen(&mut app);

        app.on_key(key(KeyCode::Char('a')));

        let everything = screen(&mut app);

        assert!(only_receipts.contains("0 of 1, receipts and invoices"));
        assert!(only_receipts.contains("Nothing here"));

        assert!(everything.contains("1 of 1, everything stored"));
        assert!(everything.contains(&format!("Document {id}  receipt.eml")));
    }

    #[test]
    fn searches_as_you_type_and_clears_with_escape() {
        let (mut app, _) = app_with_one_document();

        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Char('/')));

        typed(&mut app, "nothing like it");

        let searched = screen(&mut app);

        app.on_key(key(KeyCode::Esc));

        let cleared = screen(&mut app);

        assert!(searched.contains("0 of 1, everything stored, matching \"nothing like it\""));
        assert!(searched.contains("/nothing like it"));
        assert!(cleared.contains("1 of 1, everything stored"));
    }

    #[test]
    fn marks_it_personal_and_back_and_names_its_vendor() {
        let (mut app, id) = app_with_one_document();

        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Char('p')));

        let personal = screen(&mut app);

        app.on_key(key(KeyCode::Char('p')));

        let back = screen(&mut app);

        app.on_key(key(KeyCode::Char('v')));

        // The prompt starts with the name it has: cleared first.
        for _ in 0..80 {
            app.on_key(key(KeyCode::Backspace));
        }

        typed(&mut app, "Fjordbyrå");
        app.on_key(key(KeyCode::Enter));

        let named = screen(&mut app);

        assert!(personal.contains(&format!("Document {id} corrected")));
        assert!(personal.contains("Edited    personal"));
        assert!(!back.contains("Edited    personal"));
        assert!(named.contains("Edited    vendor Fjordbyrå"));
        assert!(named.contains("Fjordbyrå"));
    }
}
