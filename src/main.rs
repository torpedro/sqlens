mod app;
mod db;
mod ui;

use std::{
    io::{self, IsTerminal, Write},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::Parser;
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyEventKind,
    },
    execute,
};

#[derive(Parser)]
#[command(version, about = "Explore SQLite databases in a read-only terminal UI")]
struct Args {
    /// Path to an existing SQLite database
    db: PathBuf,
}

struct TerminalModes;
impl Drop for TerminalModes {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let conn = db::open(&args.db)?;
    let tables = db::tables(&conn).context("Cannot read SQLite schema")?;
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "SQLens needs an interactive terminal"
    );
    let mut app = app::App::new(tables, db::Worker::new(conn)?)?;
    ratatui::run(|terminal| -> Result<()> {
        let _modes = TerminalModes;
        execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
        let mut hits = ui::HitMap::default();
        let mut dirty = true;
        while !app.quit {
            dirty |= app.poll();
            if dirty {
                terminal.draw(|frame| hits = ui::render(frame, &mut app))?;
                dirty = false;
            }
            if !event::poll(Duration::from_millis(30))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if let Some(text) = app.key(key)? {
                        // OSC 52 also works over SSH when the terminal permits it.
                        // Base64 ensures database content cannot inject terminal controls.
                        write!(
                            io::stdout(),
                            "\x1b]52;c;{}\x07",
                            STANDARD.encode(text.as_bytes())
                        )?;
                        io::stdout().flush()?;
                        app.notify("Copy sent to terminal clipboard (OSC 52)");
                    }
                }
                Event::Paste(text) => {
                    if let Some(input) = &mut app.input {
                        input.insert(&text);
                    }
                }
                Event::Mouse(mouse) => ui::mouse(&mut app, &hits, mouse)?,
                Event::Resize(_, _) => {}
                _ => continue,
            }
            dirty = true;
        }
        Ok(())
    })
}
