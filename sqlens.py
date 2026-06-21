#!/usr/bin/env python3
import sys
import sqlite3
import subprocess
import argparse
import textwrap
import json
from pathlib import Path

from rich.text import Text

from textual.app import App, ComposeResult
from textual.screen import Screen
from textual.widgets import Input, ListView, ListItem, DataTable, Label, TextArea
from textual.containers import Horizontal, Vertical
from textual import on
from textual.binding import Binding

from sqlens_db import (
    SelectExpression,
    build_query,
    build_where,
    connect_readonly,
    get_row_count,
    get_schema,
    get_tables,
    parse_select_expression,
    quote_ident,
    validate_select_expression,
)

PAGE_SIZE = 50
COL_KEYS = "1234567890"
EXPR_KEYS = "abcdefghijklmnopqrstuvwxyz"

NUMERIC_TYPES = {"INTEGER", "REAL", "NUMERIC", "FLOAT", "DOUBLE", "INT", "NUMBER", "DECIMAL", "BIGINT"}

HELP_LIST = "↑↓ navigate  |  Enter focus table  |  Ctrl+Q quit"
HELP_TABLE = "s/S sort ▲▼  |  f LIKE filter  |  C reset  |  c copy  |  1-9 toggle col  |  + add expr  |  PgDn/PgUp page  |  Ctrl+Q quit"


def copy_to_clipboard(text: str) -> bool:
    for cmd in (
        ["clip.exe"],
        ["pbcopy"],
        ["wl-copy"],
        ["xclip", "-selection", "clipboard"],
        ["xsel", "--clipboard", "--input"],
    ):
        try:
            subprocess.run(cmd, input=text.encode(), check=True, capture_output=True)
            return True
        except (FileNotFoundError, subprocess.CalledProcessError):
            continue
    return False


def format_cell(value, col_type: str) -> Text:
    if value is None:
        return Text("NULL", style="italic dim")
    is_numeric = any(t in col_type.upper() for t in NUMERIC_TYPES)
    return Text(str(value), justify="right" if is_numeric else "left")


# ── Cell detail screen ──────────────────────────────────────────────────────────

class CellDetailScreen(Screen):
    BINDINGS = [
        Binding("escape", "go_back", "Back"),
        Binding("backspace", "go_back", "Back", priority=True),
        Binding("c", "copy", "Copy", priority=True),
        Binding("ctrl+left", "prev_col", "Ctrl+← col", show=False, priority=True),
        Binding("ctrl+right", "next_col", "Ctrl+→ col", show=False, priority=True),
        Binding("ctrl+up", "prev_row", "Ctrl+↑ row", show=False, priority=True),
        Binding("ctrl+down", "next_row", "Ctrl+↓ row", show=False, priority=True),
    ]

    def __init__(
        self,
        col_names: list[str],
        raw_rows: list[tuple],
        row_idx: int,
        col_idx: int,
    ) -> None:
        super().__init__()
        self.col_names = col_names
        self.raw_rows = raw_rows
        self.row_idx = row_idx
        self.col_idx = col_idx

        value = raw_rows[row_idx][col_idx]
        self.col_name = col_names[col_idx]
        self.text = "NULL" if value is None else str(value)
        self.parsed_json: str | None = None
        if value is not None:
            try:
                parsed = json.loads(str(value))
                self.parsed_json = json.dumps(parsed, indent=2, ensure_ascii=False)
            except (json.JSONDecodeError, ValueError):
                pass

    def compose(self) -> ComposeResult:
        n_rows, n_cols = len(self.raw_rows), len(self.col_names)
        pos = f"row {self.row_idx + 1}/{n_rows}  col {self.col_idx + 1}/{n_cols}"
        yield Label(f"{self.col_name}  [{pos}]", id="cell-detail-col")
        if self.parsed_json is not None:
            yield TextArea(
                self.parsed_json,
                language="json",
                read_only=True,
                id="cell-detail-value",
            )
        else:
            yield Label(self.text, id="cell-detail-value")

    def go_to(self, row: int, col: int) -> None:
        self.app.pop_screen()
        self.app.push_screen(CellDetailScreen(self.col_names, self.raw_rows, row, col))

    def action_go_back(self) -> None:
        self.app.pop_screen()

    def action_copy(self) -> None:
        copy_to_clipboard(self.text)

    def action_prev_col(self) -> None:
        if self.col_idx > 0:
            self.go_to(self.row_idx, self.col_idx - 1)

    def action_next_col(self) -> None:
        if self.col_idx < len(self.col_names) - 1:
            self.go_to(self.row_idx, self.col_idx + 1)

    def action_prev_row(self) -> None:
        if self.row_idx > 0:
            self.go_to(self.row_idx - 1, self.col_idx)

    def action_next_row(self) -> None:
        if self.row_idx < len(self.raw_rows) - 1:
            self.go_to(self.row_idx + 1, self.col_idx)


# ── Main screen ─────────────────────────────────────────────────────────────────

class MainScreen(Screen):
    BINDINGS = [
        Binding("pagedown", "next_page", "Next Page", priority=True),
        Binding("pageup", "prev_page", "Prev Page", priority=True),
        Binding("ctrl+q", "quit_app", "Quit"),
        Binding("ctrl+c", "quit_app", "Quit", show=False, priority=True),
    ]

    def __init__(self, conn: sqlite3.Connection) -> None:
        super().__init__()
        self.conn = conn
        # Table list state
        self.all_tables: list[str] = []
        # Data pane state
        self.current_table: str | None = None
        self.columns: list[str] = []
        self.col_types: list[str] = []
        self.hidden_columns: set[str] = set()
        self.extra_exprs: list[SelectExpression] = []
        self.sort_col: str | None = None
        self.sort_dir: str = "ASC"
        self.total_rows: int = 0
        self.current_page: int = 0
        self.col_filters: dict[str, str] = {}
        self.raw_rows: list[tuple] = []
        self.like_col: str = ""

    @property
    def visible_columns(self) -> list[str]:
        return [c for c in self.columns if c not in self.hidden_columns]

    @property
    def expression_labels(self) -> list[str]:
        return [expr.label for expr in self.extra_exprs]

    @property
    def display_columns(self) -> list[str]:
        return list(self.visible_columns) + self.expression_labels

    def compose(self) -> ComposeResult:
        with Horizontal():
            with Vertical(id="left-pane"):
                yield ListView(id="table-list")
            with Vertical(id="right-pane"):
                yield DataTable(id="schema-table", show_cursor=False)
                yield Label("", id="error-label")
                yield DataTable(id="data-table", cursor_type="cell")
                yield Label("─── Row detail ───", id="detail-divider")
                yield DataTable(id="detail-table", show_cursor=False)
                yield Label("", id="status-bar")
                yield Label("", id="query-bar")
        with Horizontal(id="expr-bar"):
            yield Label("+", id="expr-prefix")
            yield Input(
                placeholder="SELECT expression, e.g. UPPER(name) AS upper_name",
                id="expr-input",
            )
        with Horizontal(id="like-bar"):
            yield Label("", id="like-prefix")
            yield Input(placeholder="search text", id="like-input")
        yield Label(HELP_LIST, id="help-bar")

    def on_mount(self) -> None:
        self.all_tables = get_tables(self.conn)
        schema_dt = self.query_one("#schema-table", DataTable)
        schema_dt.add_columns(" ", " ", "Column", "Type")
        detail_dt = self.query_one("#detail-table", DataTable)
        detail_dt.add_columns("Column", "Value")
        self.query_one("#expr-bar").display = False
        self.query_one("#like-bar").display = False
        self.rebuild_list()
        self.query_one("#table-list", ListView).focus()

    # ── Left pane ──────────────────────────────────────────────────────────────

    def rebuild_list(self) -> None:
        lv = self.query_one("#table-list", ListView)
        lv.clear()
        for name in self.all_tables:
            lv.append(ListItem(Label(name)))
        lv.index = 0 if self.all_tables else None

    @on(ListView.Highlighted, "#table-list")
    def on_table_highlighted(self, event: ListView.Highlighted) -> None:
        self.query_one("#help-bar", Label).update(HELP_LIST)
        if event.item is None:
            return
        lv = self.query_one("#table-list", ListView)
        idx = lv.index
        if idx is not None and 0 <= idx < len(self.all_tables):
            self.load_table(self.all_tables[idx])

    @on(ListView.Selected, "#table-list")
    def on_table_selected(self, _: ListView.Selected) -> None:
        self.query_one("#data-table", DataTable).focus()

    # ── Right pane: schema ─────────────────────────────────────────────────────

    def load_table(self, table_name: str) -> None:
        if table_name == self.current_table:
            return
        self.current_table = table_name
        schema = get_schema(self.conn, table_name)
        self.columns = [col for col, _ in schema]
        self.col_types = [typ for _, typ in schema]
        self.hidden_columns = set()
        self.extra_exprs = []
        self.sort_col = None
        self.sort_dir = "ASC"
        self.current_page = 0
        self.col_filters = {}
        self.query_one("#detail-table", DataTable).clear()
        self.rebuild_schema()
        self.refresh_data()

    def rebuild_schema(self) -> None:
        schema_dt = self.query_one("#schema-table", DataTable)
        schema_dt.clear()
        for i, (col_name, col_type) in enumerate(zip(self.columns, self.col_types)):
            hidden = col_name in self.hidden_columns
            key_cell = Text(COL_KEYS[i] if i < len(COL_KEYS) else " ", style="bold cyan")
            indicator = Text("○", style="dim red") if hidden else Text("●", style="green")
            name_cell = Text(col_name, style="dim") if hidden else Text(col_name)
            type_cell = Text(col_type, style="dim italic") if hidden else Text(col_type, style="dim italic")
            schema_dt.add_row(key_cell, indicator, name_cell, type_cell)
        for i, expr in enumerate(self.extra_exprs):
            key_cell = Text(EXPR_KEYS[i] if i < len(EXPR_KEYS) else " ", style="bold magenta")
            indicator = Text("⊕", style="cyan")
            name_cell = Text(expr.label, style="italic cyan")
            type_cell = Text("expr", style="dim")
            schema_dt.add_row(key_cell, indicator, name_cell, type_cell)

    def toggle_column(self, key: str) -> None:
        idx = COL_KEYS.index(key)
        if idx >= len(self.columns):
            return
        col = self.columns[idx]
        if col in self.hidden_columns:
            self.hidden_columns.discard(col)
        else:
            if len(self.visible_columns) <= 1:
                return
            self.hidden_columns.add(col)
        self.rebuild_schema()
        self.refresh_data()

    def remove_expr(self, key: str) -> None:
        idx = EXPR_KEYS.index(key)
        if idx >= len(self.extra_exprs):
            return
        removed = self.extra_exprs.pop(idx)
        if self.sort_col == removed.label:
            self.sort_col = None
        self.rebuild_schema()
        self.refresh_data()

    # ── Right pane: expression input ───────────────────────────────────────────

    def action_add_expr(self) -> None:
        if self.current_table is None:
            return
        self.query_one("#expr-bar").display = True
        inp = self.query_one("#expr-input", Input)
        inp.value = ""
        inp.focus()

    @on(Input.Submitted, "#like-input")
    def on_like_submitted(self, event: Input.Submitted) -> None:
        self.query_one("#like-bar").display = False
        text = event.value.strip()
        if self.like_col:
            if text:
                self.col_filters[self.like_col] = text
            else:
                self.col_filters.pop(self.like_col, None)
            self.current_page = 0
            self.refresh_data()
        self.query_one("#data-table", DataTable).focus()

    @on(Input.Submitted, "#expr-input")
    def on_expr_submitted(self, event: Input.Submitted) -> None:
        raw = event.value.strip()
        if raw and self.current_table is not None:
            expr = parse_select_expression(raw)
            existing = set(self.columns) | set(self.expression_labels)
            if expr.label in existing:
                self.query_one("#error-label", Label).update(
                    f"[red]Error: duplicate column label: {expr.label}[/red]"
                )
            else:
                try:
                    validate_select_expression(self.conn, self.current_table, expr)
                except sqlite3.Error as e:
                    self.query_one("#error-label", Label).update(f"[red]Error: {e}[/red]")
                else:
                    self.extra_exprs.append(expr)
                    self.query_one("#error-label", Label).update("")
                    self.rebuild_schema()
                    self.refresh_data()
        self.query_one("#expr-bar").display = False
        self.query_one("#data-table", DataTable).focus()

    def sort_ref(self) -> str | None:
        if self.sort_col is None:
            return None
        if self.sort_col in self.columns:
            return quote_ident(self.sort_col)
        for expr in self.extra_exprs:
            if expr.label == self.sort_col:
                return expr.sort_ref
        return None

    def update_status(self) -> None:
        if self.current_table is None:
            return
        offset = self.current_page * PAGE_SIZE
        max_page = max(0, (self.total_rows - 1) // PAGE_SIZE) if self.total_rows > 0 else 0
        row_start = offset + 1 if self.total_rows > 0 else 0
        row_end = min(offset + PAGE_SIZE, self.total_rows)
        sort_hint = ""
        if self.sort_col:
            arrow = "▲" if self.sort_dir == "ASC" else "▼"
            sort_hint = f"  │  sorted by {self.sort_col} {arrow}"
        self.query_one("#status-bar", Label).update(
            f" {self.current_table}  │  "
            f"Rows {row_start}–{row_end} of {self.total_rows:,}  │  "
            f"Page {self.current_page + 1}/{max_page + 1}"
            f"{sort_hint}"
        )

    # ── Right pane: data explorer ──────────────────────────────────────────────

    def refresh_status(self) -> None:
        self.update_status()

    def refresh_data(self) -> None:
        if self.current_table is None:
            return
        dt = self.query_one("#data-table", DataTable)
        saved_coord = dt.cursor_coordinate
        error_label = self.query_one("#error-label", Label)
        offset = self.current_page * PAGE_SIZE
        visible = self.visible_columns
        col_type_map = dict(zip(self.columns, self.col_types))

        where_sql, where_params = build_where(self.col_filters, self.columns, self.extra_exprs)
        try:
            self.total_rows = get_row_count(self.conn, self.current_table, where_sql, where_params)
            error_label.update("")
        except sqlite3.Error as e:
            error_label.update(f"[red]Error: {e}[/red]")
            return

        try:
            q = build_query(
                self.current_table, visible, self.extra_exprs,
                where_sql, self.sort_ref(), self.sort_dir, PAGE_SIZE, offset,
            )
            self.raw_rows = self.conn.execute(q, where_params).fetchall()
            pane_width = self.query_one("#right-pane").size.width - 2
            wrapped = textwrap.fill(q, width=max(40, pane_width))
            self.query_one("#query-bar", Label).update(wrapped)
        except sqlite3.Error as e:
            error_label.update(f"[red]Error: {e}[/red]")
            return

        dt.clear(columns=True)
        self.query_one("#detail-table", DataTable).clear()

        col_labels = []
        for col in visible:
            label = col
            if col == self.sort_col:
                label += " ▲" if self.sort_dir == "ASC" else " ▼"
            if col in self.col_filters:
                label += " ⊘"
            col_labels.append(label)
        for expr in self.extra_exprs:
            label = expr.label
            if expr.label == self.sort_col:
                label += " ▲" if self.sort_dir == "ASC" else " ▼"
            if expr.label in self.col_filters:
                label += " ⊘"
            col_labels.append(label)
        if col_labels:
            dt.add_columns(*col_labels)

        all_col_names = self.display_columns
        for row in self.raw_rows:
            cells = [format_cell(v, col_type_map.get(col, "")) for v, col in zip(row, all_col_names)]
            dt.add_row(*cells)

        if self.raw_rows:
            dt.move_cursor(
                row=min(saved_coord.row, len(self.raw_rows) - 1),
                column=min(saved_coord.column, len(all_col_names) - 1),
                animate=False,
            )

        self.update_status()

    def on_data_table_cell_highlighted(self, event: DataTable.CellHighlighted) -> None:
        if event.data_table.id != "data-table":
            return
        self.query_one("#help-bar", Label).update(HELP_TABLE)
        detail = self.query_one("#detail-table", DataTable)
        detail.clear()
        row_index = event.coordinate.row
        if row_index < 0 or row_index >= len(self.raw_rows):
            return
        raw = self.raw_rows[row_index]
        all_col_names = self.display_columns
        for col_name, value in zip(all_col_names, raw):
            val_text = Text("NULL", style="italic dim") if value is None else Text(str(value))
            detail.add_row(col_name, val_text)

    @on(DataTable.CellSelected, "#data-table")
    def on_cell_selected(self, event: DataTable.CellSelected) -> None:
        coord = event.coordinate
        all_cols = self.display_columns
        if coord.row < len(self.raw_rows) and coord.column < len(all_cols):
            self.app.push_screen(CellDetailScreen(all_cols, self.raw_rows, coord.row, coord.column))

    @on(DataTable.HeaderSelected, "#data-table")
    def on_header_selected(self, event: DataTable.HeaderSelected) -> None:
        raw = str(event.label).strip()
        while raw.endswith(("▲", "▼", "⊘")):
            raw = raw[:-1].strip()
        if raw == self.sort_col:
            self.sort_dir = "DESC" if self.sort_dir == "ASC" else "ASC"
        else:
            self.sort_col = raw
            self.sort_dir = "ASC"
        self.current_page = 0
        self.refresh_data()

    def action_next_page(self) -> None:
        max_page = max(0, (self.total_rows - 1) // PAGE_SIZE) if self.total_rows > 0 else 0
        if self.current_page < max_page:
            self.current_page += 1
            self.refresh_data()

    def action_prev_page(self) -> None:
        if self.current_page > 0:
            self.current_page -= 1
            self.refresh_data()

    # ── Focus + key handling ───────────────────────────────────────────────────

    def on_key(self, event) -> None:
        expr_inp = self.query_one("#expr-input", Input)
        like_inp = self.query_one("#like-input", Input)
        lv = self.query_one("#table-list", ListView)

        if like_inp.has_focus:
            if event.key == "escape":
                self.query_one("#like-bar").display = False
                self.query_one("#data-table", DataTable).focus()
                event.stop()

        elif expr_inp.has_focus:
            if event.key == "escape":
                self.query_one("#expr-bar").display = False
                lv.focus()
                event.stop()

        else:
            # ListView or DataTable has focus
            if not lv.has_focus and event.key in ("escape", "backspace"):
                lv.focus()
                event.stop()
            elif event.key in ("ctrl+right", "ctrl+left"):
                dt = self.query_one("#data-table", DataTable)
                dt.scroll_relative(x=10 if event.key == "ctrl+right" else -10, animate=False)
                event.stop()
            elif event.character == "C" and self.current_table:
                self.col_filters = {}
                self.sort_col = None
                self.sort_dir = "ASC"
                self.current_page = 0
                self.refresh_data()
                event.stop()
            elif event.character == "c" and self.current_table:
                dt = self.query_one("#data-table", DataTable)
                if dt.has_focus:
                    coord = dt.cursor_coordinate
                    all_cols = self.display_columns
                    if coord.row < len(self.raw_rows) and coord.column < len(all_cols):
                        value = self.raw_rows[coord.row][coord.column]
                        text = "" if value is None else str(value)
                        status = self.query_one("#status-bar", Label)
                        if copy_to_clipboard(text):
                            status.update(f"Copied: {text[:60]}{'…' if len(text) > 60 else ''}")
                            self.set_timer(1.5, self.refresh_status)
                        else:
                            status.update("Clipboard unavailable")
                            self.set_timer(1.5, self.refresh_status)
                event.stop()
            elif event.character == "f" and self.current_table:
                dt = self.query_one("#data-table", DataTable)
                if dt.has_focus:
                    coord = dt.cursor_coordinate
                    all_cols = self.display_columns
                    if coord.column < len(all_cols):
                        col_name = all_cols[coord.column]
                        col_ref = quote_ident(col_name) if col_name in self.columns else col_name
                        self.like_col = col_name
                        self.query_one("#like-prefix", Label).update(f"{col_ref} LIKE %…%")
                        like_inp.value = self.col_filters.get(col_name, "")
                        self.query_one("#like-bar").display = True
                        like_inp.focus()
                event.stop()
            elif event.character in ("s", "S") and self.current_table:
                dt = self.query_one("#data-table", DataTable)
                if dt.has_focus:
                    col_idx = dt.cursor_column
                    all_cols = self.display_columns
                    if col_idx < len(all_cols):
                        col_name = all_cols[col_idx]
                        new_dir = "ASC" if event.character == "s" else "DESC"
                        if self.sort_col == col_name and self.sort_dir == new_dir:
                            self.sort_col = None
                        else:
                            self.sort_col = col_name
                            self.sort_dir = new_dir
                        self.current_page = 0
                        self.refresh_data()
                    event.stop()
            elif event.character == "+":
                self.action_add_expr()
                event.stop()
            elif event.character and event.character in COL_KEYS:
                self.toggle_column(event.character)
                event.stop()
            elif event.character and event.character in EXPR_KEYS and self.extra_exprs:
                self.remove_expr(event.character)
                event.stop()

    def action_quit_app(self) -> None:
        self.app.exit()


# ── App ─────────────────────────────────────────────────────────────────────────

class SQLiteBrowserApp(App):
    CSS_PATH = "sqlens.tcss"

    def __init__(self, conn: sqlite3.Connection) -> None:
        super().__init__()
        self.conn = conn

    def on_mount(self) -> None:
        self.push_screen(MainScreen(self.conn))


# ── Entry point ─────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(description="sqlens — SQLite terminal explorer")
    parser.add_argument("db", help="Path to SQLite database file")
    args = parser.parse_args()

    db_path = Path(args.db)
    if not db_path.exists():
        print(f"Error: file not found: {db_path}", file=sys.stderr)
        sys.exit(1)
    if not db_path.is_file():
        print(f"Error: not a file: {db_path}", file=sys.stderr)
        sys.exit(1)

    conn = connect_readonly(db_path)
    try:
        SQLiteBrowserApp(conn).run()
    finally:
        conn.close()


if __name__ == "__main__":
    main()
