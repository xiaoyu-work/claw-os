//! Config schema for claw-semantic.
//!
//! Currently JSON-only (we already have serde_json everywhere; adding
//! toml is a Phase-2 nicety). Path resolution mirrors XDG, falling back
//! to `~/.config/claw-semantic/config.json`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Directories to walk + watch. Tilde / $HOME are NOT expanded
    /// here — callers (the daemon main) must normalise before passing
    /// these into [`watch::Watcher`].
    pub topdirs: Vec<PathBuf>,

    /// Substring patterns that, if matched anywhere in an absolute
    /// path, cause the file to be skipped. Matches Recoll-style
    /// `skippedNames`/`skippedPaths` semantics intentionally so users
    /// only have to learn one mental model.
    pub skip_patterns: Vec<String>,

    /// Files larger than this many MB are skipped wholesale (we don't
    /// even open them). Mirrors Recoll's `filemaxmbs`.
    pub max_file_mb: u32,

    /// Approximate chunk size in characters. We chunk *by characters*
    /// not tokens because the daemon doesn't know which embedder is
    /// loaded; a Phase-2 commit that wires up `fastembed-rs` can
    /// retarget this to its tokenizer's actual budget.
    pub chunk_chars: usize,

    /// Character overlap between consecutive chunks. Helps recall when
    /// a relevant phrase straddles a chunk boundary.
    pub chunk_overlap_chars: usize,
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Self {
            topdirs: vec![
                home.join("Documents"),
                home.join("Desktop"),
                home.join("Downloads"),
            ],
            skip_patterns: vec![
                "/.git/".into(),
                "/node_modules/".into(),
                "/__pycache__/".into(),
                "/target/".into(),
                "/.venv/".into(),
                "/.cache/".into(),
                "/.recoll/".into(),
                "/.claw-semantic/".into(),
            ],
            max_file_mb: 50,
            chunk_chars: 1024,
            chunk_overlap_chars: 128,
        }
    }
}

impl Config {
    /// Resolve the config file path. Honours `XDG_CONFIG_HOME` first;
    /// otherwise `~/.config/claw-semantic/config.json`.
    pub fn path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg)
                .join("claw-semantic")
                .join("config.json");
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        home.join(".config").join("claw-semantic").join("config.json")
    }

    /// Load from disk, falling back to defaults if the file doesn't
    /// exist. A *malformed* file is an error — we don't want to
    /// silently overwrite a user's broken config with defaults.
    pub fn load_or_default() -> Result<Self> {
        let p = Self::path();
        if !p.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&p)
            .with_context(|| format!("reading {}", p.display()))?;
        let cfg: Self = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", p.display()))?;
        Ok(cfg)
    }

    /// Should this absolute path be excluded based on `skip_patterns`?
    pub fn is_skipped(&self, abs: &std::path::Path) -> bool {
        let s = abs.to_string_lossy();
        self.skip_patterns.iter().any(|p| s.contains(p))
    }
}
