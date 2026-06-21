from __future__ import annotations

import re
import sqlite3
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import quote


@dataclass(frozen=True)
class SelectExpression:
    sql: str
    label: str
    value_ref: str
    sort_ref: str


ALIASED_EXPR_RE = re.compile(
    r"^(?P<body>.+?)\s+AS\s+(?P<alias>\"(?:[^\"]|\"\")+\"|'(?:[^']|'')+'|`[^`]+`|\[[^\]]+\]|[A-Za-z_][A-Za-z0-9_]*)\s*$",
    re.IGNORECASE | re.DOTALL,
)


def quote_ident(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def connect_readonly(path: Path) -> sqlite3.Connection:
    uri = f"file:{quote(str(path.resolve()))}?mode=ro"
    return sqlite3.connect(uri, uri=True)


def get_tables(conn: sqlite3.Connection) -> list[str]:
    rows = conn.execute(
        "SELECT name FROM sqlite_master WHERE type IN ('table','view') ORDER BY name"
    ).fetchall()
    return [r[0] for r in rows]


def get_schema(conn: sqlite3.Connection, table: str) -> list[tuple[str, str]]:
    rows = conn.execute(f"PRAGMA table_info({quote_ident(table)})").fetchall()
    return [(r[1], r[2] or "-") for r in rows]


def get_row_count(
    conn: sqlite3.Connection,
    table: str,
    where_sql: str = "",
    params: tuple[object, ...] = (),
) -> int:
    q = f"SELECT COUNT(*) FROM {quote_ident(table)}"
    if where_sql.strip():
        q += f" WHERE {where_sql}"
    return conn.execute(q, params).fetchone()[0]


def build_query(
    table: str,
    visible_cols: list[str],
    extra_exprs: list[SelectExpression],
    where_sql: str,
    sort_ref: str | None,
    sort_dir: str,
    limit: int,
    offset: int,
) -> str:
    parts = [quote_ident(c) for c in visible_cols] + [expr.sql for expr in extra_exprs]
    q = f"SELECT {', '.join(parts)} FROM {quote_ident(table)}"
    if where_sql.strip():
        q += f" WHERE {where_sql}"
    if sort_ref:
        q += f" ORDER BY {sort_ref} {sort_dir}"
    q += f" LIMIT {limit} OFFSET {offset}"
    return q


def build_where(
    filters: dict[str, str],
    columns: list[str],
    expressions: list[SelectExpression],
) -> tuple[str, tuple[object, ...]]:
    parts: list[str] = []
    params: list[object] = []
    expr_by_label = {expr.label: expr for expr in expressions}
    column_set = set(columns)
    for name, text in filters.items():
        if name in column_set:
            value_ref = quote_ident(name)
        elif name in expr_by_label:
            value_ref = expr_by_label[name].value_ref
        else:
            continue
        parts.append(f"{value_ref} LIKE ?")
        params.append(f"%{text}%")
    return " AND ".join(parts), tuple(params)


def parse_select_expression(raw: str) -> SelectExpression:
    expr = raw.strip()
    match = ALIASED_EXPR_RE.match(expr)
    if not match:
        return SelectExpression(sql=expr, label=expr, value_ref=expr, sort_ref=expr)

    body = match.group("body").strip()
    alias = unquote_alias(match.group("alias").strip())
    return SelectExpression(
        sql=expr,
        label=alias,
        value_ref=body,
        sort_ref=quote_ident(alias),
    )


def validate_select_expression(
    conn: sqlite3.Connection,
    table: str,
    expression: SelectExpression,
) -> None:
    q = f"SELECT {expression.sql} FROM {quote_ident(table)} LIMIT 0"
    conn.execute(q).fetchall()


def unquote_alias(alias: str) -> str:
    if alias.startswith('"') and alias.endswith('"'):
        return alias[1:-1].replace('""', '"')
    if alias.startswith("'") and alias.endswith("'"):
        return alias[1:-1].replace("''", "'")
    if alias.startswith("[") and alias.endswith("]"):
        return alias[1:-1]
    if alias.startswith("`") and alias.endswith("`"):
        return alias[1:-1]
    return alias
