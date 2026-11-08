//! Config schema for claw-semantic.
//!
//! Currently JSON-only (we already have serde_json everywhere; adding
//! toml is a Phase-2 nicety). Path resolution mirrors XDG, falling back
//! to `~/.config/claw-semantic/config.json`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const PROTECTED_PATTERNS: &[&str] = &[
    "/.ssh/",
    "/.gnupg/",
    "/.env",
    "/.config/cos/",
    "/.local/share/cos/",
    "/.local/state/cos/",
    "/.local/state/claw-semantic/",
];

const PROTECTED_ROOTS: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/etc",
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/boot",
    "/root",
    "/var/lib/cos",
    "/var/log/cos",
    "/var/cache/cos",
];

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

    /// Chunk size in characters, bounded by the fixed local model's input
    /// limit. The concrete embedder also enforces its tokenizer budget.
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
                "/.ssh/".into(),
                "/.gnupg/".into(),
                "/.config/cos/".into(),
                "/.local/share/cos/".into(),
                "/.local/state/claw-semantic/".into(),
                "/.env".into(),
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
            return PathBuf::from(xdg).join("claw-semantic").join("config.json");
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        home.join(".config")
            .join("claw-semantic")
            .join("config.json")
    }

    /// Load from disk, falling back to defaults if the file doesn't
    /// exist. A *malformed* file is an error — we don't want to
    /// silently overwrite a user's broken config with defaults.
    pub fn load_or_default() -> Result<Self> {
        let p = Self::path();
        if !p.exists() {
            return Ok(Self::default());
        }
        let raw =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        let cfg: Self =
            serde_json::from_str(&raw).with_context(|| format!("parsing {}", p.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.topdirs.is_empty() && self.topdirs.len() <= 64,
            "file index requires 1-64 explicit source directories"
        );
        anyhow::ensure!(
            (1..=50).contains(&self.max_file_mb)
                && (64..=8192).contains(&self.chunk_chars)
                && self.chunk_overlap_chars < self.chunk_chars / 2,
            "invalid file index size or chunk limits"
        );
        anyhow::ensure!(
            self.skip_patterns.len() <= 256
                && self
                    .skip_patterns
                    .iter()
                    .all(|pattern| !pattern.is_empty() && pattern.len() <= 4096),
            "file index exclusions must be bounded, nonempty patterns"
        );
        for path in &self.topdirs {
            anyhow::ensure!(path.is_absolute() && path != std::path::Path::new("/")
                && !path.components().any(|part|matches!(part,std::path::Component::ParentDir)),
                "file index source directories must be bounded absolute paths, never the system root");
            anyhow::ensure!(
                !Self::is_protected(path),
                "file index source is a protected location: {}",
                path.display()
            );
        }
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        self.validate()?;
        let path = Self::path();
        let parent = path
            .parent()
            .context("file index config has no directory")?;
        std::fs::create_dir_all(parent)?;
        let metadata = std::fs::symlink_metadata(parent)?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "file index configuration directory is unsafe"
        );
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        let temporary = parent.join(format!("config-{}.tmp", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        let result = (|| {
            file.write_all(&serde_json::to_vec(self)?)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)?;
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() && temporary.exists() {
            std::fs::remove_file(&temporary)
                .context("remove failed file-index configuration write")?;
        }
        result
    }

    /// Should this absolute path be excluded based on `skip_patterns`?
    pub fn is_skipped(&self, abs: &std::path::Path) -> bool {
        let s = abs.to_string_lossy();
        Self::is_protected(abs) || self.skip_patterns.iter().any(|p| s.contains(p))
    }

    fn is_protected(path: &std::path::Path) -> bool {
        let text = format!("{}/", path.to_string_lossy());
        PROTECTED_ROOTS.iter().any(|root| path.starts_with(root))
            || PROTECTED_PATTERNS
                .iter()
                .any(|pattern| text.contains(pattern))
    }
}
