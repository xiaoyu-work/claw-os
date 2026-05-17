//! Filesystem watcher: walks topdirs on startup, then subscribes to
//! create / modify / delete events via the `notify` crate (inotify on
//! Linux).
//!
//! This module owns *no* embedding or storage logic — it just emits
//! [`FsEvent`]s on a channel for the daemon to consume.

use anyhow::{Context, Result};
use notify::{event::EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum FsEvent {
    Upsert(PathBuf),
    Delete(PathBuf),
}

pub struct Watcher {
    _inner: RecommendedWatcher,
    pub rx: mpsc::Receiver<FsEvent>,
    pub topdirs: Vec<PathBuf>,
}

impl Watcher {
    pub fn spawn(topdirs: Vec<PathBuf>) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<FsEvent>();
        let tx_clone = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                let kind = ev.kind;
                for p in ev.paths {
                    let msg = match &kind {
                        EventKind::Create(_) | EventKind::Modify(_) => FsEvent::Upsert(p),
                        EventKind::Remove(_) => FsEvent::Delete(p),
                        _ => continue,
                    };
                    let _ = tx_clone.send(msg);
                }
            }
        })
        .context("constructing fs watcher")?;
        for d in &topdirs {
            if d.exists() {
                watcher
                    .watch(d, RecursiveMode::Recursive)
                    .with_context(|| format!("watching {}", d.display()))?;
            } else {
                tracing::warn!(path = %d.display(), "topdir missing, not watching");
            }
        }
        Ok(Self {
            _inner: watcher,
            rx,
            topdirs,
        })
    }

    /// Block until the next event, with a debounce timeout so the
    /// daemon can flush batched work periodically even when idle.
    pub fn recv_with_timeout(&self, dur: Duration) -> Option<FsEvent> {
        self.rx.recv_timeout(dur).ok()
    }
}

/// Synchronous walk of every file under `topdirs`. Yields paths that
/// should be considered for upsert at startup.
pub fn walk(topdirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in topdirs {
        if !d.exists() {
            continue;
        }
        walk_into(d, &mut out);
    }
    out
}

fn walk_into(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if let Ok(meta) = entry.metadata() {
            if meta.is_dir() {
                walk_into(&p, out);
            } else if meta.is_file() {
                out.push(p);
            }
        }
    }
}
