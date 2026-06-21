# sqlens

A terminal UI for exploring SQLite databases.

## Installation

```bash
uv install
```

## Usage

```bash
uv run sqlens path/to/database.sqlite
```

## Interface

The screen is split into two panes:

- **Left** — table/view list. Navigate with `↑`/`↓`, press `Enter` to focus the data table.
- **Right** — schema, data table, row detail, and query bar.

### Data table hotkeys

| Key | Action |
|-----|--------|
| `s` / `S` | Sort focused column ASC / DESC (press again to clear) |
| `f` | LIKE filter on focused column (`%text%`) |
| `C` | Reset all filters and sort |
| `c` | Copy focused cell value to clipboard |
| `1`–`9`, `0` | Toggle visibility of columns 1–10 |
| `+` | Add a custom SQL SELECT expression as an extra column |
| `a`–`z` | Remove custom expression (when expressions exist) |
| `Enter` | Open full cell detail view |
| `PgDn` / `PgUp` | Next / previous page (50 rows per page) |
| `Esc` / `Backspace` | Return focus to table list |
| `Ctrl+Q` / `Ctrl+C` | Quit |

### Column indicators

- `●` — visible column
- `○` — hidden column
- `⊘` — active LIKE filter on this column
- `▲` / `▼` — current sort column and direction

### Cell detail view

Press `Enter` on any cell to open a full-screen detail view. JSON values are automatically detected and rendered with syntax highlighting.

| Key | Action |
|-----|--------|
| `Ctrl+←` / `Ctrl+→` | Previous / next column (same row) |
| `Ctrl+↑` / `Ctrl+↓` | Previous / next row (same column) |
| `c` | Copy value to clipboard |
| `Esc` / `Backspace` | Back to table |

### LIKE filter

Press `f` on a focused cell to open a filter input for that column. Entering text filters rows where `column LIKE '%text%'`. Submit empty to clear the filter for that column. Active filters are AND-ed together.

### Custom expressions

Press `+` to add a SQL SELECT expression as an extra column (e.g. `UPPER(name) AS upper_name`, `json_extract(data, '$.id') AS id`). Press the assigned letter key (`a`, `b`, …) to remove it.

## Requirements

- Python 3.10+
- [Textual](https://github.com/Textualize/textual) 8.2.7+
