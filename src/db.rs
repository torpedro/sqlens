use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, params_from_iter, types::ValueRef};

pub const PAGE_SIZE: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub kind: String,
    pub primary_key: bool,
    pub generated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expression {
    pub body: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub table: String,
    pub hidden: BTreeSet<String>,
    pub expressions: Vec<Expression>,
    pub filters: BTreeMap<String, String>,
    pub sort: Option<(String, bool)>, // true = descending
    pub page: usize,
}

impl Query {
    pub fn new(table: String) -> Self {
        Self {
            table,
            hidden: BTreeSet::new(),
            expressions: vec![],
            filters: BTreeMap::new(),
            sort: None,
            page: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub fn text(&self) -> String {
        match self {
            Self::Null => "NULL".into(),
            Self::Integer(n) => n.to_string(),
            Self::Real(n) => n.to_string(),
            Self::Text(s) => s.clone(),
            Self::Blob(bytes) => {
                let mut s = String::from("X'");
                use std::fmt::Write;
                for byte in bytes {
                    let _ = write!(s, "{byte:02X}");
                }
                s.push('\'');
                s
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Page {
    pub query: Query,
    pub schema: Vec<Column>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    pub total: usize,
    pub sql: String,
    pub params: Vec<String>,
}

pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub fn open(path: &Path) -> Result<Connection> {
    ensure!(path.is_file(), "Not a database file: {}", path.display());
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("Cannot open {}", path.display()))?;
    conn.busy_timeout(Duration::from_millis(200))?;
    conn.execute_batch("PRAGMA query_only = ON; PRAGMA trusted_schema = OFF;")?;
    Ok(conn)
}

pub fn tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type IN ('table', 'view') ORDER BY name")?;
    Ok(stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn schema(conn: &Connection, table: &str) -> Result<Vec<Column>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_xinfo({})", quote_ident(table)))?;
    let cols = stmt
        .query_map([], |r| {
            let hidden: i32 = r.get(6)?;
            Ok((
                hidden,
                Column {
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    primary_key: r.get::<_, i32>(5)? != 0,
                    generated: hidden == 2 || hidden == 3,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cols
        .into_iter()
        .filter(|(hidden, _)| *hidden != 1)
        .map(|(_, col)| col)
        .collect())
}

// Find an outer AS token, ignoring nested expressions, quoted identifiers,
// string literals and SQL comments. SQLite remains the expression validator.
fn alias_start(sql: &str) -> Option<usize> {
    let b = sql.as_bytes();
    let (mut i, mut depth, mut alias) = (0, 0usize, None);
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' | b'`' | b'[' => {
                let end = if b[i] == b'[' { b']' } else { b[i] };
                i += 1;
                while i < b.len() {
                    if b[i] == end {
                        i += 1;
                        if end != b']' && i < b.len() && b[i] == end {
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && &b[i..i + 2] != b"*/" {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                if depth == 0 && sql[start..i].eq_ignore_ascii_case("AS") {
                    alias = Some(start);
                }
            }
            _ => i += 1,
        }
    }
    alias
}

fn expression(
    conn: &Connection,
    table: &str,
    raw: &str,
    existing: &[String],
) -> Result<Expression> {
    let raw = raw.trim();
    ensure!(!raw.is_empty(), "Enter a SELECT expression");
    let (body, label) = if let Some(pos) = alias_start(raw) {
        // Preparing the original projection lets SQLite decode quoted aliases.
        let stmt = conn.prepare(&format!(
            "SELECT {raw}\nFROM {} LIMIT 0",
            quote_ident(table)
        ))?;
        ensure!(
            stmt.column_count() == 1,
            "An expression must return exactly one column"
        );
        (&raw[..pos], stmt.column_name(0)?.to_string())
    } else {
        (raw, raw.to_string())
    };
    let body = body.trim().to_string();
    let stmt = conn.prepare(&format!(
        "SELECT ({body}\n) FROM {} LIMIT 0",
        quote_ident(table)
    ))?;
    ensure!(
        stmt.readonly() && stmt.column_count() == 1,
        "Enter one read-only scalar expression"
    );
    ensure!(
        !existing.iter().any(|s| s.eq_ignore_ascii_case(&label)),
        "Duplicate column label: {label}"
    );
    Ok(Expression { body, label })
}

#[derive(Default)]
struct CountCache {
    key: String,
    params: Vec<String>,
    version: i64,
    count: usize,
}

fn load(
    conn: &Connection,
    mut query: Query,
    add: Option<&str>,
    cache: &mut CountCache,
) -> Result<Page> {
    let schema = schema(conn, &query.table)?;
    ensure!(!schema.is_empty(), "No visible columns in {}", query.table);
    query
        .hidden
        .retain(|name| schema.iter().any(|c| &c.name == name));
    let mut names: Vec<String> = schema.iter().map(|c| c.name.clone()).collect();
    names.extend(query.expressions.iter().map(|e| e.label.clone()));
    if let Some(raw) = add {
        query
            .expressions
            .push(expression(conn, &query.table, raw, &names)?);
    }
    let mut projection: Vec<String> = schema.iter().map(|c| quote_ident(&c.name)).collect();
    projection.extend(
        query
            .expressions
            .iter()
            .map(|e| format!("({}\n) AS {}", e.body, quote_ident(&e.label))),
    );
    let base = format!(
        "SELECT {} FROM {}",
        projection.join(", "),
        quote_ident(&query.table)
    );
    let mut columns: Vec<String> = schema
        .iter()
        .filter(|c| !query.hidden.contains(&c.name))
        .map(|c| c.name.clone())
        .collect();
    columns.extend(query.expressions.iter().map(|e| e.label.clone()));
    ensure!(!columns.is_empty(), "Keep at least one column visible");
    let all: BTreeSet<_> = schema
        .iter()
        .map(|c| &c.name)
        .chain(query.expressions.iter().map(|e| &e.label))
        .collect();
    query.filters.retain(|name, _| all.contains(name));
    if query
        .sort
        .as_ref()
        .is_some_and(|(name, _)| !all.contains(name))
    {
        query.sort = None;
    }
    let params: Vec<String> = query.filters.values().map(|v| format!("%{v}%")).collect();
    let conditions: Vec<String> = query
        .filters
        .keys()
        .map(|c| format!("{} LIKE ?", quote_ident(c)))
        .collect();
    let suffix = if conditions.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", conditions.join(" AND "))
    };
    // Filter projected aliases in an outer query, preserving expression precedence.
    let source = format!("({base}){suffix}");
    let count_sql = format!("SELECT COUNT(*) FROM {source}");
    let version: i64 = conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
    let total = if cache.key == count_sql && cache.params == params && cache.version == version {
        cache.count
    } else {
        let count: usize =
            conn.query_row(&count_sql, params_from_iter(params.iter()), |r| r.get(0))?;
        *cache = CountCache {
            key: count_sql,
            params: params.clone(),
            version,
            count,
        };
        count
    };
    query.page = query.page.min(total.saturating_sub(1) / PAGE_SIZE);
    let order = match &query.sort {
        Some((name, desc)) if all.contains(name) => format!(
            " ORDER BY {} {}",
            quote_ident(name),
            if *desc { "DESC" } else { "ASC" }
        ),
        _ => String::new(),
    };
    let sql = format!(
        "SELECT {} FROM {source}{order} LIMIT {PAGE_SIZE} OFFSET {}",
        columns
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", "),
        query.page * PAGE_SIZE
    );
    let mut stmt = conn.prepare(&sql)?;
    ensure!(
        stmt.column_count() == columns.len(),
        "Query column count changed unexpectedly"
    );
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |r| {
            (0..columns.len())
                .map(|i| {
                    Ok(match r.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(v) => Value::Integer(v),
                        ValueRef::Real(v) => Value::Real(v),
                        ValueRef::Text(v) => Value::Text(String::from_utf8_lossy(v).into_owned()),
                        ValueRef::Blob(v) => Value::Blob(v.to_vec()),
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Page {
        query,
        schema,
        columns,
        rows,
        total,
        sql,
        params,
    })
}

struct Request {
    id: u64,
    query: Query,
    add: Option<String>,
}
pub struct Reply {
    pub id: u64,
    pub result: Result<Page>,
}

pub struct Worker {
    tx: Option<Sender<Request>>,
    pub rx: Receiver<Reply>,
    generation: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn new(conn: Connection) -> Result<Self> {
        let (tx, requests) = mpsc::channel::<Request>();
        let (responses, rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let current = Arc::clone(&generation);
        let thread = thread::Builder::new()
            .name("sqlite".into())
            .spawn(move || {
                let mut cache = CountCache::default();
                while let Ok(mut request) = requests.recv() {
                    // Coalesce navigation requests before doing any database work.
                    while let Ok(newer) = requests.try_recv() {
                        request = newer;
                    }
                    if current.load(Ordering::Relaxed) != request.id {
                        continue;
                    }
                    let check = Arc::clone(&current);
                    let id = request.id;
                    conn.progress_handler(1000, Some(move || check.load(Ordering::Relaxed) != id));
                    let result = load(&conn, request.query, request.add.as_deref(), &mut cache);
                    if responses.send(Reply { id, result }).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            rx,
            generation,
            thread: Some(thread),
        })
    }

    pub fn request(&self, query: Query, add: Option<String>) -> Result<u64> {
        let id = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.tx
            .as_ref()
            .context("Database worker stopped")?
            .send(Request { id, query, add })?;
        Ok(id)
    }

    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel();
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, n INTEGER GENERATED ALWAYS AS (id*2)); INSERT INTO t(id,name) VALUES (1,'alpha'), (2,'beta'), (3,NULL);").unwrap();
        conn
    }

    #[test]
    fn generated_columns_and_typed_values() {
        let page = load(
            &fixture(),
            Query::new("t".into()),
            None,
            &mut CountCache::default(),
        )
        .unwrap();
        assert!(page.schema[2].generated);
        assert_eq!(page.rows[0][2], Value::Integer(2));
        assert_eq!(page.rows[2][1], Value::Null);
    }

    #[test]
    fn expressions_are_scalar_and_labels_are_unambiguous() {
        let conn = fixture();
        for raw in [
            "id, name",
            "*",
            "id FROM t",
            "id; DELETE FROM t",
            "id AS NAME",
        ] {
            assert!(
                load(
                    &conn,
                    Query::new("t".into()),
                    Some(raw),
                    &mut CountCache::default()
                )
                .is_err(),
                "{raw}"
            );
        }
        for raw in [
            "CAST(id AS TEXT) AS \"cast value\"",
            "' AS ' AS [space]",
            "id /* AS nope */ AS `double`",
            "id + 1",
            "json_extract('{\"id\":2}', '$.id') AS json_id",
        ] {
            let p = load(
                &conn,
                Query::new("t".into()),
                Some(raw),
                &mut CountCache::default(),
            )
            .unwrap();
            assert_eq!(p.columns.len(), 4);
            assert_eq!(p.rows[0].len(), 4);
        }
    }

    #[test]
    fn filters_bind_values_and_preserve_boolean_expression_precedence() {
        let conn = fixture();
        let mut page = load(
            &conn,
            Query::new("t".into()),
            Some("id = 1 OR id = 2 AS matches"),
            &mut CountCache::default(),
        )
        .unwrap();
        page.query.filters.insert("matches".into(), "0".into());
        page.query.hidden.insert("id".into());
        let page = load(&conn, page.query, None, &mut CountCache::default()).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0][1], Value::Integer(6));
        let mut query = Query::new("t".into());
        query.filters.insert("name".into(), "' OR 1=1 --".into());
        assert_eq!(
            load(&conn, query, None, &mut CountCache::default())
                .unwrap()
                .total,
            0
        );
    }

    #[test]
    fn pagination_sort_and_clamping() {
        let conn = fixture();
        conn.execute_batch("WITH RECURSIVE n(x) AS (VALUES(4) UNION ALL SELECT x+1 FROM n WHERE x<120) INSERT INTO t(id,name) SELECT x, 'row' FROM n;").unwrap();
        let mut q = Query::new("t".into());
        q.sort = Some(("id".into(), true));
        q.page = 1;
        let p = load(&conn, q.clone(), None, &mut CountCache::default()).unwrap();
        assert_eq!(p.rows[0][0], Value::Integer(70));
        q.page = 999;
        let p = load(&conn, q, None, &mut CountCache::default()).unwrap();
        assert_eq!(p.query.page, 2);
        assert_eq!(p.rows.len(), 20);
    }

    #[test]
    fn readonly_and_quoted_identifiers() {
        let dir = tempfile::tempdir().unwrap();
        // Windows forbids '?' in filenames. Keep spaces and '#' coverage there,
        // and exercise literal '?' handling on platforms that support it.
        let filename = if cfg!(windows) {
            "data #.sqlite"
        } else {
            "data ?#.sqlite"
        };
        let path = dir.path().join(filename);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE \"a\"\"b\" (\"x\"\"y\" TEXT); INSERT INTO \"a\"\"b\" VALUES ('[bold]literal[/bold]');").unwrap();
        drop(conn);
        let conn = open(&path).unwrap();
        assert!(conn.execute("DELETE FROM \"a\"\"b\"", []).is_err());
        let p = load(
            &conn,
            Query::new("a\"b".into()),
            None,
            &mut CountCache::default(),
        )
        .unwrap();
        assert_eq!(p.rows[0][0].text(), "[bold]literal[/bold]");
    }

    #[test]
    fn count_cache_tracks_external_writes_and_aggregate_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.sqlite");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch("CREATE TABLE t(id INTEGER); INSERT INTO t VALUES(1),(2);")
            .unwrap();
        let reader = open(&path).unwrap();
        let mut cache = CountCache::default();
        let q = Query::new("t".into());
        assert_eq!(load(&reader, q.clone(), None, &mut cache).unwrap().total, 2);
        writer.execute("INSERT INTO t VALUES(3)", []).unwrap();
        assert_eq!(load(&reader, q.clone(), None, &mut cache).unwrap().total, 3);
        let page = load(&reader, q, Some("COUNT(*) AS count"), &mut cache).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0][1], Value::Integer(3));
    }

    #[test]
    fn views_blobs_and_empty_results() {
        let conn = fixture();
        conn.execute_batch("CREATE VIEW v AS SELECT id, X'00FF' AS bytes FROM t;")
            .unwrap();
        let page = load(
            &conn,
            Query::new("v".into()),
            None,
            &mut CountCache::default(),
        )
        .unwrap();
        assert_eq!(page.rows[0][1], Value::Blob(vec![0, 255]));
        assert_eq!(page.rows[0][1].text(), "X'00FF'");
        let mut q = Query::new("v".into());
        q.filters.insert("id".into(), "no match".into());
        q.page = 100;
        let page = load(&conn, q, None, &mut CountCache::default()).unwrap();
        assert_eq!(page.total, 0);
        assert_eq!(page.query.page, 0);
        assert!(page.rows.is_empty());
    }

    #[test]
    fn worker_interrupts_expensive_query_for_new_request() {
        let conn = fixture();
        conn.execute_batch("CREATE VIEW slow AS WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) AS n FROM n;").unwrap();
        let worker = Worker::new(conn).unwrap();
        worker.request(Query::new("slow".into()), None).unwrap();
        assert!(worker.rx.recv_timeout(Duration::from_millis(20)).is_err());
        let id = worker.request(Query::new("t".into()), None).unwrap();
        loop {
            let reply = worker.rx.recv_timeout(Duration::from_secs(3)).unwrap();
            if reply.id == id {
                assert_eq!(reply.result.unwrap().total, 3);
                break;
            }
        }
    }
}
