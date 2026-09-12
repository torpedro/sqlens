//! Generate the screenshot fixture without requiring the sqlite3 command-line tool.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use rusqlite::Connection;

const FIXTURE: &str = include_str!("demo.sql");

#[derive(Parser)]
#[command(about = "Create a fictional shop database for SQLens screenshots")]
struct Args {
    /// New database path; existing files are never overwritten
    #[arg(default_value = "demo.sqlite")]
    output: PathBuf,
}

fn create_demo(path: &Path) -> Result<()> {
    ensure!(!path.exists(), "Refusing to overwrite {}", path.display());
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("Cannot create a database in {}", parent.display()))?;
    {
        let mut conn = Connection::open(temporary.path())?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        let transaction = conn.transaction()?;
        transaction.execute_batch(FIXTURE)?;
        transaction.commit()?;
        conn.close().map_err(|(_, error)| error)?;
    }
    // Publish only a complete database, and refuse overwrites even if another
    // process creates the destination after the initial existence check.
    temporary.persist_noclobber(path).with_context(|| {
        format!(
            "Cannot save {} (destination must not exist)",
            path.display()
        )
    })?;
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    create_demo(&args.output)?;
    println!(
        "Created {}: 120 customers, 8 products, 360 orders, and 2 views.",
        args.output.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_is_complete_and_existing_files_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.sqlite");
        create_demo(&path).unwrap();
        {
            let conn = Connection::open(&path).unwrap();
            for (table, expected) in [
                ("customers", 120),
                ("products", 8),
                ("orders", 360),
                ("recent_orders", 90),
                ("customer_totals", 120),
            ] {
                let count: i64 = conn
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                    .unwrap();
                assert_eq!(count, expected, "{table}");
            }
            let plan: String = conn
                .query_row(
                    "SELECT json_extract(profile, '$.plan') FROM customers WHERE id = 3",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(plan, "team");
            let total: f64 = conn
                .query_row("SELECT total FROM orders WHERE id = 2", [], |r| r.get(0))
                .unwrap();
            assert_eq!(total, 48.0);
            let violations: i64 = conn
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(violations, 0);
        }
        let original = std::fs::read(&path).unwrap();
        assert!(
            create_demo(&path)
                .unwrap_err()
                .to_string()
                .contains("Refusing to overwrite")
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}
