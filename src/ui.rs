use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, Focus, InputKind},
    db::{PAGE_SIZE, Value},
};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;

#[derive(Default)]
pub struct HitMap {
    pub tables: Rect,
    pub grid: Rect,
    pub columns: Vec<(u16, u16, usize)>,
}

fn block(title: impl Into<String>, active: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .title(title.into())
        .border_style(Style::default().fg(if active { ACCENT } else { MUTED }))
}

// Keep raw data intact; control characters are made visible only when rendering.
pub fn literal(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\t' => '⇥',
            '\r' => '␍',
            '\n' => '↵',
            c if c.is_control() => '�',
            c => c,
        })
        .collect()
}

fn preview(value: &Value) -> String {
    match value {
        Value::Blob(bytes) => format!("<BLOB: {} bytes>", bytes.len()),
        Value::Text(text) => {
            let short: String = text.chars().take(160).collect();
            let mut s = literal(&short);
            if text.chars().nth(160).is_some() {
                s.push('…');
            }
            s
        }
        _ => value.text(),
    }
}

pub fn render(frame: &mut Frame, app: &mut App) -> HitMap {
    let area = frame.area();
    if area.width < 40 || area.height < 12 {
        frame.render_widget(
            Paragraph::new("SQLens — resize to at least 40×12. Ctrl+Q quits."),
            area,
        );
        return HitMap::default();
    }
    if app.detail {
        render_detail(frame, app, area);
        return HitMap::default();
    }
    let vertical = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(area);
    let split = Layout::horizontal([
        Constraint::Length((area.width / 4).clamp(15, 30)),
        Constraint::Min(1),
    ])
    .split(vertical[0]);
    let list_block = block(" Tables / views ", app.focus == Focus::Tables);
    let list_area = list_block.inner(split[0]);
    let items: Vec<_> = app
        .tables
        .iter()
        .map(|s| ListItem::new(literal(s)))
        .collect();
    frame.render_stateful_widget(
        List::new(items)
            .block(list_block)
            .highlight_style(Style::default().fg(Color::Black).bg(ACCENT)),
        split[0],
        &mut app.list,
    );
    let mut hits = HitMap {
        tables: list_area,
        ..Default::default()
    };
    if let Some(page) = &app.page {
        let schema_height = if area.height >= 24 {
            (page.schema.len() + page.query.expressions.len() + 2).min(6) as u16
        } else {
            3
        };
        let detail_height = if area.height >= 24 { 4 } else { 0 };
        let parts = Layout::vertical([
            Constraint::Length(schema_height),
            Constraint::Min(3),
            Constraint::Length(detail_height),
            Constraint::Length(3),
        ])
        .split(split[1]);
        let mut schema_lines: Vec<Line> = page
            .schema
            .iter()
            .enumerate()
            .map(|(i, col)| {
                let key = if i < 10 {
                    ((i + 1) % 10).to_string()
                } else {
                    " ".into()
                };
                let hidden = page.query.hidden.contains(&col.name);
                Line::from(vec![
                    Span::styled(
                        format!("{key} {} ", if hidden { "○" } else { "●" }),
                        Style::default().fg(if hidden { MUTED } else { ACCENT }),
                    ),
                    Span::raw(format!(
                        "{}  {}{}{}",
                        literal(&col.name),
                        literal(&col.kind),
                        if col.primary_key { " PK" } else { "" },
                        if col.generated { " generated" } else { "" }
                    )),
                ])
            })
            .collect();
        schema_lines.extend(page.query.expressions.iter().map(|e| {
            Line::styled(
                format!("  + {}", literal(&e.label)),
                Style::default().fg(Color::Magenta),
            )
        }));
        frame.render_widget(
            Paragraph::new(schema_lines).block(block(" Schema · v: all columns ", false)),
            parts[0],
        );
        let grid_block = block(
            format!(
                " {}{} ",
                literal(&page.query.table),
                if app.loading { " · loading…" } else { "" }
            ),
            app.focus == Focus::Grid,
        );
        let grid_area = grid_block.inner(parts[1]);
        hits.grid = grid_area;
        let available = grid_area.width.max(1);
        let widths: Vec<u16> = page
            .columns
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let content = page
                    .rows
                    .iter()
                    .map(|r| UnicodeWidthStr::width(preview(&r[i]).as_str()))
                    .max()
                    .unwrap_or(0);
                (content
                    .max(UnicodeWidthStr::width(name.as_str()) + 3)
                    .clamp(8, 40) as u16)
                    .min(available)
            })
            .collect();
        app.first_col = app.first_col.min(app.col);
        while app.first_col < app.col
            && widths[app.first_col..=app.col]
                .iter()
                .map(|w| *w as usize + 1)
                .sum::<usize>()
                .saturating_sub(1)
                > available as usize
        {
            app.first_col += 1;
        }
        let mut end = app.first_col;
        let mut used = 0u16;
        while end < widths.len() {
            let width = widths[end];
            let gap = u16::from(end != app.first_col);
            if used.saturating_add(gap).saturating_add(width) > available {
                break;
            }
            let x = grid_area.x + used + gap;
            hits.columns.push((x, width, end));
            used += gap + width;
            end += 1;
        }
        end = end.max((app.first_col + 1).min(widths.len()));
        let columns = app.first_col..end;
        let header: Vec<Cell> = columns
            .clone()
            .map(|i| {
                let name = &page.columns[i];
                let mut label = literal(name);
                if let Some((sort, desc)) = &page.query.sort
                    && name == sort
                {
                    label.push_str(if *desc { " ▼" } else { " ▲" });
                }
                if page.query.filters.contains_key(name) {
                    label.push_str(" ⊘");
                }
                Cell::from(label).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            })
            .collect();
        let rows: Vec<Row> = page
            .rows
            .iter()
            .map(|row| {
                Row::new(
                    columns
                        .clone()
                        .map(|i| {
                            let value = &row[i];
                            let numeric = matches!(value, Value::Integer(_) | Value::Real(_));
                            let text = Line::from(preview(value)).alignment(if numeric {
                                Alignment::Right
                            } else {
                                Alignment::Left
                            });
                            Cell::from(text).style(if matches!(value, Value::Null) {
                                Style::default().fg(MUTED).add_modifier(Modifier::ITALIC)
                            } else {
                                Style::default()
                            })
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        app.grid.select_cell(if page.rows.is_empty() {
            None
        } else {
            Some((app.row, app.col - app.first_col))
        });
        let table = Table::new(
            rows,
            widths[columns].iter().copied().map(Constraint::Length),
        )
        .header(Row::new(header))
        .column_spacing(1)
        .block(grid_block)
        .cell_highlight_style(Style::default().fg(Color::Black).bg(ACCENT));
        frame.render_stateful_widget(table, parts[1], &mut app.grid);
        if page.rows.is_empty() {
            frame.render_widget(
                Paragraph::new("No matching rows").style(Style::default().fg(MUTED)),
                Rect::new(
                    grid_area.x,
                    grid_area.y + 1,
                    grid_area.width,
                    grid_area.height.saturating_sub(1),
                ),
            );
        }
        if detail_height > 0 {
            let lines: Vec<Line> = page
                .rows
                .get(app.row)
                .map(|row| {
                    page.columns
                        .iter()
                        .zip(row)
                        .map(|(name, value)| {
                            Line::from(vec![
                                Span::styled(
                                    format!("{}: ", literal(name)),
                                    Style::default().fg(ACCENT),
                                ),
                                Span::raw(preview(value)),
                            ])
                        })
                        .collect()
                })
                .unwrap_or_default();
            frame.render_widget(
                Paragraph::new(lines)
                    .scroll((app.col.min(u16::MAX as usize) as u16, 0))
                    .block(block(" Row detail · Enter: full value ", false)),
                parts[2],
            );
        }
        let params = if page.params.is_empty() {
            String::new()
        } else {
            format!("  params: {:?}", page.params)
        };
        frame.render_widget(
            Paragraph::new(format!("{}{}", literal(&page.sql), literal(&params)))
                .wrap(Wrap { trim: false })
                .block(block(" SQL ", false)),
            parts[3],
        );
    } else {
        let message = if app.loading {
            "Loading…"
        } else if app.tables.is_empty() {
            "This database has no tables or views."
        } else {
            "Select another table or press Tab, then r to retry."
        };
        frame.render_widget(
            Paragraph::new(message).block(block(" SQLens ", false)),
            split[1],
        );
    }
    let status = if let Some(error) = &app.error {
        format!("Error: {}", literal(error))
    } else if let Some((notice, _)) = &app.notice {
        literal(notice)
    } else if app.loading {
        "Loading… · Esc from grid cancels · table navigation stays available".into()
    } else if let Some(page) = &app.page {
        let start = if page.total == 0 {
            0
        } else {
            page.query.page * PAGE_SIZE + 1
        };
        format!(
            "Rows {start}–{} of {} · Page {}/{} · Column {}/{} · READ ONLY",
            page.query.page * PAGE_SIZE + page.rows.len(),
            page.total,
            page.query.page + 1,
            page.total.saturating_sub(1) / PAGE_SIZE + 1,
            app.col + 1,
            page.columns.len()
        )
    } else {
        "READ ONLY".into()
    };
    let help = if app.focus == Focus::Tables {
        "↑↓ tables · Enter/Tab grid · ? help · Ctrl+Q quit"
    } else {
        "s/S sort · f filter · + expr · Del remove · v columns · c copy · PgUp/Dn · ? help"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                status,
                Style::default().fg(if app.error.is_some() {
                    Color::Red
                } else {
                    ACCENT
                }),
            ),
            Line::raw(help),
        ]),
        vertical[1],
    );
    if let Some(input) = &app.input {
        let title = match &input.kind {
            InputKind::Filter(name) => {
                format!(" Filter {} · Enter applies · Esc cancels ", literal(name))
            }
            InputKind::Expression => " SELECT expression · Enter adds · Esc cancels ".into(),
        };
        let popup = centered(area, area.width.saturating_sub(6).min(100), 5);
        frame.render_widget(Clear, popup);
        let b = block(title, true);
        let inner = b.inner(popup);
        // Scroll by characters while measuring terminal cells for the cursor.
        let chars: Vec<char> = input.text.chars().collect();
        let mut start = 0;
        while start < input.cursor
            && UnicodeWidthStr::width(
                chars[start..input.cursor]
                    .iter()
                    .collect::<String>()
                    .as_str(),
            ) >= inner.width.saturating_sub(1) as usize
        {
            start += 1;
        }
        let visible: String = chars[start..].iter().collect();
        let cursor_width = UnicodeWidthStr::width(
            chars[start..input.cursor]
                .iter()
                .collect::<String>()
                .as_str(),
        ) as u16;
        frame.render_widget(Paragraph::new(visible).block(b), popup);
        frame.set_cursor_position((inner.x + cursor_width, inner.y));
    }
    if let Some(index) = app.chooser {
        let popup = centered(area, 65.min(area.width - 4), area.height.saturating_sub(4));
        frame.render_widget(Clear, popup);
        if let Some(page) = &app.page {
            let items: Vec<_> = page
                .schema
                .iter()
                .map(|col| {
                    ListItem::new(format!(
                        "{} {}  {}",
                        if page.query.hidden.contains(&col.name) {
                            "○"
                        } else {
                            "●"
                        },
                        literal(&col.name),
                        literal(&col.kind)
                    ))
                })
                .collect();
            app.schema_list.select(Some(index));
            frame.render_stateful_widget(
                List::new(items)
                    .block(block(" Columns · ↑↓ · Space toggles · Esc closes ", true))
                    .highlight_style(Style::default().bg(Color::DarkGray)),
                popup,
                &mut app.schema_list,
            );
        }
    }
    if app.help {
        let popup = centered(area, 78.min(area.width - 2), 23.min(area.height - 2));
        frame.render_widget(Clear, popup);
        frame.render_widget(Paragraph::new("Tab                  Switch tables / grid\nArrows               Navigate tables or cells\nEnter                Focus grid / open full cell detail\ns / S                Sort ascending / descending; repeat clears\nf                    LIKE filter selected column; empty clears\nC                    Clear filters and sort\n1–9, 0               Toggle first ten base columns\nv                    Choose visibility of any base column\n+                    Add one SELECT expression (optional AS alias)\nDelete               Remove selected expression and its filter\nc                    Copy raw cell via terminal clipboard (OSC 52)\nPgDn / PgUp          Next / previous 50-row page\nr                    Reload current table data\nEsc / Backspace      Back to table list; cancel active query\n\nCell detail: arrows scroll; Ctrl+arrows change cell; c copies.\nInput: arrows/Home/End edit; Ctrl+U clears; paste supported.\nMouse: click tables/cells/headers; wheel navigates.\nCtrl+Q / Ctrl+C      Quit from any screen\n? / Esc              Close help").wrap(Wrap { trim: false }).block(block(" SQLens keyboard help ", true)), popup);
    }
    hits
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn json_line(line: &str) -> Line<'static> {
    let mut spans = vec![];
    let chars: Vec<_> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        let color = if chars[i] == '"' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i = (i + 2).min(chars.len());
                } else if chars[i] == '"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            if chars[i..].iter().find(|c| !c.is_whitespace()) == Some(&':') {
                ACCENT
            } else {
                Color::Green
            }
        } else if chars[i].is_ascii_digit() || chars[i] == '-' {
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_digit() || matches!(chars[i], '.' | 'e' | 'E' | '+' | '-'))
            {
                i += 1;
            }
            Color::Yellow
        } else if chars[i].is_ascii_alphabetic() {
            i += 1;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            Color::Magenta
        } else {
            i += 1;
            Color::White
        };
        spans.push(Span::styled(
            chars[start..i].iter().collect::<String>(),
            Style::default().fg(color),
        ));
    }
    Line::from(spans)
}

fn render_detail(frame: &mut Frame, app: &mut App, area: Rect) {
    let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
    let Some(value) = app.cell() else {
        return;
    };
    let raw = value.text();
    let parsed = if matches!(value, Value::Text(_)) {
        serde_json::from_str::<serde_json::Value>(&raw).ok()
    } else {
        None
    };
    let lines = if let Some(json) = parsed {
        serde_json::to_string_pretty(&json)
            .unwrap_or(raw)
            .lines()
            .map(json_line)
            .collect::<Vec<_>>()
    } else {
        raw.lines()
            .map(|s| Line::raw(literal(s)))
            .collect::<Vec<_>>()
    };
    let max_y = lines
        .len()
        .saturating_sub(parts[0].height.saturating_sub(2) as usize)
        .min(u16::MAX as usize) as u16;
    let max_x = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0)
        .saturating_sub(parts[0].width.saturating_sub(2) as usize)
        .min(u16::MAX as usize) as u16;
    app.detail_scroll.0 = app.detail_scroll.0.min(max_y);
    app.detail_scroll.1 = app.detail_scroll.1.min(max_x);
    let title = format!(
        " {} · row {} · column {} ",
        literal(&app.column().unwrap_or_default()),
        app.query.as_ref().map_or(0, |q| q.page * PAGE_SIZE) + app.row + 1,
        app.col + 1
    );
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll(app.detail_scroll)
            .block(block(title, true)),
        parts[0],
    );
    let footer = app.notice.as_ref().map_or(
        "Arrows scroll · Ctrl+arrows change cell · c copy · Esc back · Ctrl+Q quit",
        |(text, _)| text,
    );
    frame.render_widget(Paragraph::new(footer), parts[1]);
}

pub fn mouse(app: &mut App, hits: &HitMap, event: MouseEvent) -> anyhow::Result<()> {
    if app.help || app.input.is_some() || app.chooser.is_some() {
        return Ok(());
    }
    if app.detail {
        match event.kind {
            MouseEventKind::ScrollUp => app.detail_scroll.0 = app.detail_scroll.0.saturating_sub(3),
            MouseEventKind::ScrollDown => {
                app.detail_scroll.0 = app.detail_scroll.0.saturating_add(3)
            }
            _ => {}
        }
        return Ok(());
    }
    let position = (event.column, event.row).into();
    if hits.tables.contains(position) {
        let selected = app.list.selected().unwrap_or(0);
        let index = match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                Some(app.list.offset() + (event.row - hits.tables.y) as usize)
            }
            MouseEventKind::ScrollUp => Some(selected.saturating_sub(1)),
            MouseEventKind::ScrollDown => {
                Some((selected + 1).min(app.tables.len().saturating_sub(1)))
            }
            _ => None,
        };
        if let Some(i) = index {
            app.focus = Focus::Tables;
            app.select_table(i)?;
        }
    } else if hits.grid.contains(position) && !app.loading {
        app.focus = Focus::Grid;
        match event.kind {
            MouseEventKind::ScrollUp => app.move_cell(-3, 0),
            MouseEventKind::ScrollDown => app.move_cell(3, 0),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((_, _, col)) = hits
                    .columns
                    .iter()
                    .find(|(x, width, _)| event.column >= *x && event.column < *x + *width)
                {
                    app.col = *col;
                    if event.row == hits.grid.y {
                        let desc = app
                            .query
                            .as_ref()
                            .and_then(|q| q.sort.as_ref())
                            .is_some_and(|(name, desc)| {
                                Some(name) == app.page.as_ref().and_then(|p| p.columns.get(*col))
                                    && !desc
                            });
                        app.sort(desc)?;
                    } else {
                        app.row = app.grid.offset() + (event.row - hits.grid.y - 1) as usize;
                        app.clamp_selection();
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn wide_grid_follows_selection_and_mouse_selects_visible_column() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let definitions = (0..20)
            .map(|i| format!("column_{i} TEXT"))
            .collect::<Vec<_>>()
            .join(",");
        let values = (0..20)
            .map(|i| format!("'value for column {i}'"))
            .collect::<Vec<_>>()
            .join(",");
        conn.execute_batch(&format!(
            "CREATE TABLE wide({definitions}); INSERT INTO wide VALUES({values});"
        ))
        .unwrap();
        let mut app = App::new(vec!["wide".into()], crate::db::Worker::new(conn).unwrap()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while app.loading {
            assert!(std::time::Instant::now() < deadline);
            app.poll();
            std::thread::yield_now();
        }
        app.col = 19;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut hits = HitMap::default();
        terminal.draw(|f| hits = render(f, &mut app)).unwrap();
        assert!(app.first_col > 0);
        let (x, _, _) = *hits.columns.iter().find(|(_, _, col)| *col == 19).unwrap();
        app.col = 0;
        mouse(
            &mut app,
            &hits,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: hits.grid.y + 1,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
        )
        .unwrap();
        assert_eq!(app.col, 19);
        app.chooser = Some(19);
        terminal
            .draw(|f| {
                render(f, &mut app);
            })
            .unwrap();
        app.chooser = None;
        app.input = Some(crate::app::Input::new(
            InputKind::Expression,
            "界".repeat(100),
        ));
        terminal
            .draw(|f| {
                render(f, &mut app);
            })
            .unwrap();
    }

    #[test]
    fn render_literal_values_modals_and_small_terminals() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE t(value TEXT); INSERT INTO t VALUES('[bold]literal[/bold]');",
        )
        .unwrap();
        let mut app = App::new(vec!["t".into()], crate::db::Worker::new(conn).unwrap()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while app.loading {
            assert!(std::time::Instant::now() < deadline);
            app.poll();
            std::thread::yield_now();
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|f| {
                render(f, &mut app);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("[bold]literal[/bold]"));
        assert!(text.contains("READ ONLY"));
        app.detail = true;
        terminal
            .draw(|f| {
                render(f, &mut app);
            })
            .unwrap();
        app.detail = false;
        app.help = true;
        terminal
            .draw(|f| {
                render(f, &mut app);
            })
            .unwrap();
        for (w, h) in [(40, 12), (20, 5), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| {
                    render(f, &mut app);
                })
                .unwrap();
        }
    }
}
