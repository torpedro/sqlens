use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{ListState, TableState};

use crate::db::{PAGE_SIZE, Page, Query, Value, Worker};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tables,
    Grid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputKind {
    Filter(String),
    Expression,
}

#[derive(Clone, Debug)]
pub struct Input {
    pub kind: InputKind,
    pub text: String,
    pub cursor: usize, // Unicode scalar index, never a byte offset
}

impl Input {
    pub fn new(kind: InputKind, text: String) -> Self {
        let cursor = text.chars().count();
        Self { kind, text, cursor }
    }
    fn byte_index(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(i, _)| i)
    }
    pub fn insert(&mut self, s: &str) {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.byte_index(), &clean);
        self.cursor += clean.chars().count();
    }
    pub fn key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.text.clear();
                self.cursor = 0;
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => self.cursor = 0,
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cursor = self.text.chars().count()
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.insert(&c.to_string())
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.text.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.chars().count(),
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.byte_index());
            }
            KeyCode::Delete if self.cursor < self.text.chars().count() => {
                self.text.remove(self.byte_index());
            }
            _ => {}
        }
    }
}

pub struct App {
    pub tables: Vec<String>,
    pub list: ListState,
    pub grid: TableState,
    pub focus: Focus,
    pub query: Option<Query>,
    pub page: Option<Page>,
    pub row: usize,
    pub col: usize,
    pub first_col: usize,
    pub input: Option<Input>,
    pub detail: bool,
    pub detail_scroll: (u16, u16),
    pub help: bool,
    pub chooser: Option<usize>,
    pub schema_list: ListState,
    pub loading: bool,
    pub error: Option<String>,
    pub notice: Option<(String, Instant)>,
    pub quit: bool,
    pub worker: Worker,
    pending: Option<u64>,
    retry_input: Option<Input>,
}

impl App {
    pub fn new(tables: Vec<String>, worker: Worker) -> Result<Self> {
        let mut app = Self {
            tables,
            list: ListState::default(),
            grid: TableState::default(),
            focus: Focus::Tables,
            query: None,
            page: None,
            row: 0,
            col: 0,
            first_col: 0,
            input: None,
            detail: false,
            detail_scroll: (0, 0),
            help: false,
            chooser: None,
            schema_list: ListState::default(),
            loading: false,
            error: None,
            notice: None,
            quit: false,
            worker,
            pending: None,
            retry_input: None,
        };
        if !app.tables.is_empty() {
            app.select_table(0)?;
        }
        Ok(app)
    }

    fn submit(&mut self, query: Query, add: Option<String>) -> Result<()> {
        self.pending = Some(self.worker.request(query.clone(), add)?);
        self.query = Some(query);
        self.loading = true;
        self.error = None;
        self.retry_input = None;
        Ok(())
    }

    pub fn select_table(&mut self, index: usize) -> Result<()> {
        if index >= self.tables.len() {
            return Ok(());
        }
        if self.list.selected() == Some(index) {
            return Ok(());
        }
        self.list.select(Some(index));
        self.page = None;
        self.row = 0;
        self.col = 0;
        self.first_col = 0;
        self.grid = TableState::default();
        self.submit(Query::new(self.tables[index].clone()), None)
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(reply) = self.worker.rx.try_recv() {
            if self.pending != Some(reply.id) {
                continue;
            }
            self.pending = None;
            self.loading = false;
            changed = true;
            match reply.result {
                Ok(page) => {
                    self.retry_input = None;
                    if let Some(name) = self.column()
                        && let Some(index) = page.columns.iter().position(|c| c == &name)
                    {
                        self.col = index;
                    }
                    self.query = Some(page.query.clone());
                    self.page = Some(page);
                    self.clamp_selection();
                }
                Err(error) => {
                    self.error = Some(format!("{error:#}"));
                    self.input = self.retry_input.take();
                    if let Some(page) = &self.page {
                        self.query = Some(page.query.clone());
                    }
                }
            }
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|(_, until)| Instant::now() >= *until)
        {
            self.notice = None;
            changed = true;
        }
        changed
    }

    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now() + Duration::from_secs(3)));
    }

    pub fn clamp_selection(&mut self) {
        if let Some(page) = &self.page {
            self.row = self.row.min(page.rows.len().saturating_sub(1));
            self.col = self.col.min(page.columns.len().saturating_sub(1));
            self.first_col = self.first_col.min(self.col);
        }
    }

    pub fn cell(&self) -> Option<&Value> {
        if self.loading {
            return None;
        }
        self.page.as_ref()?.rows.get(self.row)?.get(self.col)
    }

    pub fn column(&self) -> Option<String> {
        self.page.as_ref()?.columns.get(self.col).cloned()
    }

    pub fn move_cell(&mut self, rows: isize, cols: isize) {
        self.row = self.row.saturating_add_signed(rows);
        self.col = self.col.saturating_add_signed(cols);
        self.clamp_selection();
        self.detail_scroll = (0, 0);
    }

    pub fn sort(&mut self, descending: bool) -> Result<()> {
        if let (Some(mut q), Some(name)) = (self.query.clone(), self.column()) {
            let sort = Some((name, descending));
            q.sort = if q.sort == sort { None } else { sort };
            q.page = 0;
            self.row = 0;
            self.submit(q, None)?;
        }
        Ok(())
    }

    pub fn toggle_column(&mut self, index: usize) -> Result<()> {
        let (Some(page), Some(mut q)) = (&self.page, self.query.clone()) else {
            return Ok(());
        };
        let Some(column) = page.schema.get(index) else {
            return Ok(());
        };
        if !q.hidden.remove(&column.name) {
            if page.schema.len() - q.hidden.len() <= 1 {
                self.notify("Keep at least one base column visible");
                return Ok(());
            }
            q.hidden.insert(column.name.clone());
        }
        self.submit(q, None)
    }

    pub fn remove_expression(&mut self) -> Result<()> {
        let (Some(mut q), Some(name)) = (self.query.clone(), self.column()) else {
            return Ok(());
        };
        if !q.expressions.iter().any(|e| e.label == name) {
            self.notify("Select an expression column to remove it");
            return Ok(());
        }
        q.expressions.retain(|e| e.label != name);
        q.filters.remove(&name);
        if q.sort.as_ref().is_some_and(|(col, _)| col == &name) {
            q.sort = None;
        }
        q.page = 0;
        self.submit(q, None)
    }

    // Returns clipboard content to the terminal owner, keeping I/O out of state transitions.
    pub fn key(&mut self, key: KeyEvent) -> Result<Option<String>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('c' | 'q')) {
            self.quit = true;
            self.worker.cancel();
            return Ok(None);
        }
        if self.help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter) {
                self.help = false;
            }
            return Ok(None);
        }
        if self.input.is_some() {
            match key.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Enter => {
                    let input = self.input.take().unwrap();
                    let retry = input.clone();
                    if let Some(mut q) = self.query.clone() {
                        match input.kind {
                            InputKind::Filter(name) => {
                                if input.text.is_empty() {
                                    q.filters.remove(&name);
                                } else {
                                    q.filters.insert(name, input.text);
                                }
                                q.page = 0;
                                self.row = 0;
                                self.submit(q, None)?;
                            }
                            InputKind::Expression if !input.text.trim().is_empty() => {
                                self.submit(q, Some(input.text))?
                            }
                            _ => {}
                        }
                        if self.loading {
                            self.retry_input = Some(retry);
                        }
                    }
                }
                _ => self.input.as_mut().unwrap().key(key),
            }
            return Ok(None);
        }
        if let Some(index) = self.chooser {
            let len = self.page.as_ref().map_or(0, |p| p.schema.len());
            match key.code {
                KeyCode::Esc | KeyCode::Char('v') => self.chooser = None,
                KeyCode::Up => self.chooser = Some(index.saturating_sub(1)),
                KeyCode::Down => self.chooser = Some((index + 1).min(len.saturating_sub(1))),
                KeyCode::Enter | KeyCode::Char(' ') if !self.loading => {
                    self.toggle_column(index)?
                }
                _ => {}
            }
            return Ok(None);
        }
        if self.detail {
            match key.code {
                KeyCode::Esc | KeyCode::Backspace => self.detail = false,
                KeyCode::Left if ctrl => self.move_cell(0, -1),
                KeyCode::Right if ctrl => self.move_cell(0, 1),
                KeyCode::Up if ctrl => self.move_cell(-1, 0),
                KeyCode::Down if ctrl => self.move_cell(1, 0),
                KeyCode::Up => self.detail_scroll.0 = self.detail_scroll.0.saturating_sub(1),
                KeyCode::Down => self.detail_scroll.0 = self.detail_scroll.0.saturating_add(1),
                KeyCode::PageUp => self.detail_scroll.0 = self.detail_scroll.0.saturating_sub(15),
                KeyCode::PageDown => self.detail_scroll.0 = self.detail_scroll.0.saturating_add(15),
                KeyCode::Left => self.detail_scroll.1 = self.detail_scroll.1.saturating_sub(4),
                KeyCode::Right => self.detail_scroll.1 = self.detail_scroll.1.saturating_add(4),
                KeyCode::Home => self.detail_scroll = (0, 0),
                KeyCode::Char('c') => return Ok(self.cell().map(Value::text)),
                _ => {}
            }
            return Ok(None);
        }
        if key.code == KeyCode::Char('?') {
            self.help = true;
            return Ok(None);
        }
        if key.code == KeyCode::Tab {
            self.focus = if self.focus == Focus::Tables {
                Focus::Grid
            } else {
                Focus::Tables
            };
            return Ok(None);
        }
        if self.focus == Focus::Tables {
            let selected = self.list.selected().unwrap_or(0);
            match key.code {
                KeyCode::Up => self.select_table(selected.saturating_sub(1))?,
                KeyCode::Down => {
                    self.select_table((selected + 1).min(self.tables.len().saturating_sub(1)))?
                }
                KeyCode::Home => self.select_table(0)?,
                KeyCode::End => self.select_table(self.tables.len().saturating_sub(1))?,
                KeyCode::Enter | KeyCode::Right => self.focus = Focus::Grid,
                _ => {}
            }
            return Ok(None);
        }
        if matches!(key.code, KeyCode::Esc | KeyCode::Backspace) {
            self.focus = Focus::Tables;
            if self.loading {
                self.worker.cancel();
                self.pending = None;
                self.loading = false;
                if let Some(page) = &self.page {
                    self.query = Some(page.query.clone());
                }
                self.notify("Query cancelled");
            }
            return Ok(None);
        }
        if self.loading {
            return Ok(None);
        }
        match key.code {
            KeyCode::Up => self.move_cell(-1, 0),
            KeyCode::Down => self.move_cell(1, 0),
            KeyCode::Left => self.move_cell(0, -1),
            KeyCode::Right => self.move_cell(0, 1),
            KeyCode::Home => self.row = 0,
            KeyCode::End => {
                self.row = self
                    .page
                    .as_ref()
                    .map_or(0, |p| p.rows.len().saturating_sub(1))
            }
            KeyCode::Enter if self.cell().is_some() => {
                self.detail = true;
                self.detail_scroll = (0, 0);
            }
            KeyCode::Char('c') => return Ok(self.cell().map(Value::text)),
            KeyCode::Char('s') => self.sort(false)?,
            KeyCode::Char('S') => self.sort(true)?,
            KeyCode::Char('f') => {
                if let (Some(q), Some(name)) = (&self.query, self.column()) {
                    let value = q.filters.get(&name).cloned().unwrap_or_default();
                    self.input = Some(Input::new(InputKind::Filter(name), value));
                }
            }
            KeyCode::Char('+') if self.query.is_some() => {
                self.input = Some(Input::new(InputKind::Expression, String::new()))
            }
            KeyCode::Delete => self.remove_expression()?,
            KeyCode::Char('v') if self.page.is_some() => self.chooser = Some(0),
            KeyCode::Char(c @ '0'..='9') => self.toggle_column(if c == '0' {
                9
            } else {
                c as usize - '1' as usize
            })?,
            KeyCode::Char('C') => {
                if let Some(mut q) = self.query.clone() {
                    q.filters.clear();
                    q.sort = None;
                    q.page = 0;
                    self.submit(q, None)?;
                }
            }
            KeyCode::Char('r') => {
                if let Some(q) = self.query.clone() {
                    self.submit(q, None)?;
                }
            }
            KeyCode::PageDown | KeyCode::PageUp => {
                if let (Some(mut q), Some(page)) = (self.query.clone(), &self.page) {
                    let next = if key.code == KeyCode::PageDown {
                        (q.page + 1).min(page.total.saturating_sub(1) / PAGE_SIZE)
                    } else {
                        q.page.saturating_sub(1)
                    };
                    if next != q.page {
                        q.page = next;
                        self.row = 0;
                        self.submit(q, None)?;
                    }
                }
            }
            _ => {}
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    pub fn fixture() -> App {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t(id INTEGER, name TEXT); INSERT INTO t VALUES(1,'[bold]literal[/bold]'),(2,'βeta');").unwrap();
        let mut app = App::new(vec!["t".into()], Worker::new(conn).unwrap()).unwrap();
        settle(&mut app);
        app
    }
    pub fn settle(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.loading {
            assert!(Instant::now() < deadline);
            app.poll();
            std::thread::yield_now();
        }
        assert!(app.error.is_none(), "{:?}", app.error);
    }
    fn press(app: &mut App, code: KeyCode) {
        app.key(KeyEvent::new(code, KeyModifiers::NONE)).unwrap();
    }

    #[test]
    fn unicode_input_edits_at_character_boundaries() {
        let mut input = Input::new(InputKind::Expression, "a界🙂".into());
        input.key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        input.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        input.insert("é");
        assert_eq!(input.text, "aé🙂");
        input.key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(input.text, "aé");
    }

    #[test]
    fn filter_expression_remove_and_navigation() {
        let mut app = fixture();
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('+'));
        app.input.as_mut().unwrap().insert("id*2 AS double");
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        app.col = 2;
        press(&mut app, KeyCode::Char('f'));
        app.input.as_mut().unwrap().insert("4");
        press(&mut app, KeyCode::Enter);
        settle(&mut app);
        assert_eq!(app.page.as_ref().unwrap().total, 1);
        press(&mut app, KeyCode::Char('s'));
        settle(&mut app);
        press(&mut app, KeyCode::Delete);
        settle(&mut app);
        let q = app.query.as_ref().unwrap();
        assert!(q.filters.is_empty());
        assert!(q.sort.is_none());
        assert!(q.expressions.is_empty());
        assert_eq!(app.page.as_ref().unwrap().total, 2);
        press(&mut app, KeyCode::Enter);
        assert!(app.detail);
        press(&mut app, KeyCode::Esc);
        assert!(!app.detail);
    }

    #[test]
    fn hiding_earlier_column_preserves_selected_column_and_last_column() {
        let mut app = fixture();
        app.focus = Focus::Grid;
        app.col = 1;
        press(&mut app, KeyCode::Char('1'));
        settle(&mut app);
        assert_eq!(app.col, 0);
        assert_eq!(app.column().as_deref(), Some("name"));
        press(&mut app, KeyCode::Char('2'));
        assert!(!app.loading);
        assert_eq!(app.page.as_ref().unwrap().columns.len(), 1);
    }

    #[test]
    fn failed_expression_preserves_last_successful_page() {
        let mut app = fixture();
        app.focus = Focus::Grid;
        press(&mut app, KeyCode::Char('+'));
        app.input.as_mut().unwrap().insert("id,name");
        press(&mut app, KeyCode::Enter);
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.loading {
            assert!(Instant::now() < deadline);
            app.poll();
            std::thread::yield_now();
        }
        assert!(app.error.is_some());
        assert_eq!(app.input.as_ref().unwrap().text, "id,name");
        assert_eq!(app.page.as_ref().unwrap().columns.len(), 2);
        assert!(app.query.as_ref().unwrap().expressions.is_empty());
    }

    #[test]
    fn copy_does_not_remove_expression_and_quit_works_in_dialog() {
        let mut app = fixture();
        app.focus = Focus::Grid;
        app.col = 1;
        assert_eq!(
            app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE))
                .unwrap(),
            Some("[bold]literal[/bold]".into())
        );
        press(&mut app, KeyCode::Char('+'));
        app.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(app.quit);
    }
}
