# sqlens

A read-only SQLite database explorer for the terminal, built in Rust with
[Ratatui](https://ratatui.rs/). Browse tables and views, sort and filter columns,
add SQL expressions, and inspect full cell values with JSON highlighting.

## Install and run

From this repository, with Rust 1.88+ and a C compiler installed:

```console
cargo install --path . --locked
sqlens path/to/database.sqlite
```

Or run directly from the checkout:

```console
cargo run --locked --release -- path/to/database.sqlite
```

SQLite is bundled into the executable; Python and a system SQLite installation
are not needed. The binary is also available at `target/release/sqlens` after
`cargo build --locked --release`. Build separately for each operating system and
architecture you distribute to.

SQLens opens existing databases with SQLite's read-only flag and enables
`query_only`. Database queries run on a dedicated worker thread. Switching tables
supersedes and interrupts old queries; quitting also interrupts database work.

## Interface

The left pane lists tables and views. The right pane contains the schema, data
grid, selected row detail, and generated SQL with its bound filter parameters.
Arrow keys move the selection; the grid scrolls horizontally to keep the selected
column visible. Click tables, cells, or headers with the mouse; the wheel navigates.
Small terminals use a compact layout; at least 40 columns × 12 rows are required.

| Key | Action |
| --- | --- |
| `Tab` | Switch between table list and grid |
| `↑` / `↓` | Navigate tables or rows |
| `←` / `→` | Navigate columns; Left from the first visible column returns to the table list |
| `Enter` | Focus the grid / open full cell detail |
| `s` / `S` | Sort selected column ascending / descending; repeat to clear |
| `f` | Edit selected column's LIKE filter |
| `C` | Clear all filters and sort |
| `1`–`9`, `0` | Toggle base columns 1–10 |
| `v` | Open a selector for all base columns; `Space` toggles visibility |
| `+` | Add a SELECT expression |
| `Delete` | Remove selected expression, including its filter and sort |
| `c` | Copy the raw selected value |
| `PgDn` / `PgUp` | Next / previous page, 50 rows per page |
| `Home` / `End` | First / last row on the page, or first / last table |
| `r` | Reload current table data |
| `Esc` / `Backspace` | Return to table list; cancel a loading query from the grid |
| `?` | Show keyboard help |
| `Ctrl+Q` / `Ctrl+C` | Quit from any screen or input |

Grid actions apply while the grid has focus. At least one base column remains
visible. Generated columns are included in the schema; internal hidden columns of
virtual tables are omitted.

### Filters and expressions

Filters use `column LIKE '%text%'` with bound parameters. Multiple filters are
AND-ed together. `%` and `_` retain SQLite's LIKE wildcard behavior. Submit empty
text to clear a filter; spaces are preserved. Hidden columns can retain filters.

Expressions must produce one column. An explicit `AS` alias is optional:

```sql
UPPER(name) AS upper_name
json_extract(data, '$.id') AS json_id
CAST(price AS REAL) * quantity AS total
```

SQLite validates expressions and decodes aliases. Duplicate labels are rejected
case-insensitively. Filters are applied to projected expression values, preserving
operator precedence. Aggregate expressions such as `COUNT(*)` follow SQLite's
semantics and can collapse the result to one row; counts reflect that result.

Inputs support Unicode typing, bracketed paste, arrows, Home/End, Delete,
Backspace, and `Ctrl+U` to clear. `Enter` submits and `Esc` cancels.

### Cell detail and clipboard

Full cell detail preserves literal text and pretty-prints JSON with syntax colors.
Arrow keys scroll, PgUp/PgDn scroll vertically, and `Ctrl+←/→/↑/↓` navigate adjacent
cells on the current page. `Esc` returns to the grid. BLOBs show their byte count
in the grid and hexadecimal SQL literals in detail. NULL is displayed and copied
as `NULL`.

Clipboard copying uses the terminal's OSC 52 protocol, including over SSH when
supported. Your terminal must permit clipboard writes. SQLens reports that it
sent the copy request; the protocol does not confirm that the clipboard changed.

### Query behavior

Counts are cached across paging, sorting, and visibility changes, and invalidated
when the query source, filters, or SQLite data version changes. Exact counts and
deep OFFSET pages may still take time on large databases. Navigation and quitting
remain responsive while SQL executes.

Without an explicit sort, row order follows SQLite's query plan. Duplicate sort
values and databases modified by other processes can produce changing page
boundaries. `r` refreshes rows; reopen SQLens to refresh the table list.

## Demo database and screenshots

Generate a repeatable database of fictional shop data from the repository root:

```console
cargo run --locked --example create_demo
cargo run --locked --release -- demo.sqlite
```

The generator uses the readable fixture in `examples/demo.sql` and bundled SQLite;
no separate SQLite command-line tool is needed. It creates `demo.sqlite` with
120 customers, 8 products, 360 orders, and two views. The data includes JSON
profiles, Unicode names, NULLs, multiline text, BLOBs, and generated order totals.
Dates and values are fixed so screenshots can be reproduced.

An existing file is never overwritten. To create another copy, supply a new path
(its parent directory must already exist):

```console
cargo run --locked --example create_demo -- another-demo.sqlite
```

For an overview screenshot, use a terminal around 120×36, select `customers`,
press Enter, and press `s` on `id` to sort ascending. For a JSON screenshot,
navigate to `profile` and press Enter. Try `json_extract(profile, '$.plan') AS plan`
as an expression, or open `orders` to see the generated `total` column.
Generated `.sqlite` files are ignored by Git; the SQL fixture and generator are tracked.

## Development

```console
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

- `src/main.rs`: CLI, terminal lifecycle, events, clipboard.
- `src/app.rs`: application state, keyboard commands, input editing.
- `src/db.rs`: read-only SQLite access, query construction, cancellable worker.
- `src/ui.rs`: Ratatui rendering, JSON highlighting, mouse hit testing.

Tests use temporary/in-memory databases and Ratatui's headless backend. They cover
read-only enforcement, generated columns, expressions, filters, paging, worker
requests, keyboard state, Unicode input, and rendering at different terminal sizes.

The Python/Textual implementation was replaced in version 0.2.0 and remains
available in Git history. A screenshot of the current Rust interface is available
in `docs/assets/screenshot-1.png` and on the documentation page.
