//! Terminal context snapshot for AI prompts.
//!
//! Port of `aterm-ai/src/contextCollector.ts` + the context-file writing logic
//! in `aiMiddleware.ts` (`submitToShell`).
//!
//! Strategy difference from aterm: aterm maintains a rolling ANSI-stripped
//! `contextBuffer` because xterm.js's grid isn't directly accessible. cosmic-term
//! owns `alacritty_terminal::Term`, so we scrape the grid directly when a prompt
//! is submitted — gives us cleaner text without ANSI strip heuristics.

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use serde::Serialize;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// JSON payload written to `$COS_AI_TMP/ac-<id>.json` for the shell function to
/// pass to copilot. Mirrors aterm's `contextData` shape (aiMiddleware.ts:288-302).
#[derive(Serialize)]
pub struct ContextSnapshot {
    /// Recent terminal output lines, newest last. Already plain text (ANSI-free).
    pub scrollback: String,

    /// Current working directory of the shell, if known.
    pub cwd: Option<String>,
}

/// Snapshot the last `max_lines` lines from a Term's visible grid + scrollback,
/// converted to plain text with trailing whitespace trimmed.
pub fn capture_context<T: EventListener>(
    term: &Term<T>,
    cwd: Option<&Path>,
    max_lines: usize,
) -> ContextSnapshot {
    let grid = term.grid();
    let total_lines = grid.total_lines();
    let columns = grid.columns();
    let take = max_lines.min(total_lines);
    let start = total_lines.saturating_sub(take);

    let mut lines: Vec<String> = Vec::with_capacity(take);
    for line_idx in start..total_lines {
        let mut buf = String::with_capacity(columns);
        let line = alacritty_terminal::index::Line(line_idx as i32 - grid.history_size() as i32);
        for col in 0..columns {
            let cell = &grid[line][alacritty_terminal::index::Column(col)];
            let c = cell.c;
            if c == '\0' {
                buf.push(' ');
            } else {
                buf.push(c);
            }
        }
        // Trim only trailing spaces — preserve any leading layout (table output, etc.).
        let trimmed = buf.trim_end().to_string();
        lines.push(trimmed);
    }

    // Drop trailing blank lines (often the empty area below the last prompt).
    while lines.last().map_or(false, |l| l.is_empty()) {
        lines.pop();
    }

    ContextSnapshot {
        scrollback: lines.join("\n"),
        cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
    }
}

/// Write a [`ContextSnapshot`] to disk at the location expected by the shell
/// function — `<dir>/ac-<id>.json`.
pub fn write_snapshot(dir: &Path, id: &str, snapshot: &ContextSnapshot) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("ac-{id}.json"));
    let mut f = fs::File::create(&path)?;
    let json = serde_json::to_string(snapshot)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    f.write_all(json.as_bytes())?;
    Ok(path)
}

/// Write the captured prompt text to `<dir>/aq-<id>.txt`.
pub fn write_query(dir: &Path, id: &str, query: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("aq-{id}.txt"));
    let mut f = fs::File::create(&path)?;
    f.write_all(query.as_bytes())?;
    Ok(path)
}
