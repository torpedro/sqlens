use std::process::Command;

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sqlens"))
}

#[test]
fn help_and_version_do_not_need_a_database_or_terminal() {
    let help = command().arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("read-only"));
    let version = command().arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn missing_and_corrupt_databases_fail_without_terminal_escape_sequences() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.sqlite");
    let output = command().arg(&missing).output().unwrap();
    assert!(!output.status.success());
    assert!(!missing.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Not a database file"));
    let corrupt = dir.path().join("corrupt.sqlite");
    std::fs::write(&corrupt, "not a SQLite database").unwrap();
    let output = command().arg(&corrupt).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Cannot read SQLite schema"));
    assert!(!output.stdout.contains(&0x1b));
}

#[test]
fn redirected_output_is_rejected_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("valid.sqlite");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("CREATE TABLE t(id INTEGER)")
        .unwrap();
    let output = command().arg(&path).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    assert!(!output.stdout.contains(&0x1b));
}
