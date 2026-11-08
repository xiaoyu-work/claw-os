//! Filesystem walker + change watcher.
//!
//! Walks topdirs on startup, then subscribes to create / modify /
//! delete events via the `notify` crate (inotify on Linux, FSEvents
//! on macOS).
//!
//! This module owns *no* embedding or storage logic — it emits
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
    Error(String),
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
                    if tx_clone.send(msg).is_err() {
                        tracing::warn!("file watcher receiver closed");
                        return;
                    }
                }
            } else if let Err(error) = res {
                if tx_clone.send(FsEvent::Error(error.to_string())).is_err() {
                    tracing::warn!("file watcher receiver closed after an error");
                }
            }
        })
        .context("constructing fs watcher")?;
        let mut watched = Vec::new();
        for d in &topdirs {
            if d.exists() {
                watcher
                    .watch(d, RecursiveMode::Recursive)
                    .with_context(|| format!("watching {}", d.display()))?;
                watched.push(d.clone());
            } else {
                tracing::warn!(path = %d.display(), "topdir missing, not watching");
            }
        }
        Ok(Self {
            _inner: watcher,
            rx,
            topdirs: watched,
        })
    }

    /// Block until the next event, with a debounce timeout so the
    /// daemon can flush batched work periodically even when idle.
    pub fn recv_with_timeout(&self, dur: Duration) -> Option<FsEvent> {
        match self.rx.recv_timeout(dur) {
            Ok(event) => Some(event),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Some(FsEvent::Error("file watcher channel disconnected".into()))
            }
        }
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

pub fn walk_checked(topdirs: &[PathBuf], maximum: usize) -> Result<Vec<PathBuf>> {
    walk_filtered(topdirs, maximum, &|_| false)
}

pub fn walk_filtered(
    topdirs: &[PathBuf],
    maximum: usize,
    excluded: &dyn Fn(&Path) -> bool,
) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    let mut pending = topdirs.to_vec();
    let mut directories = 0;
    while let Some(directory) = pending.pop() {
        if excluded(&directory) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(path=%directory.display(),"index source directory is absent");
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", directory.display()))
            }
        };
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "index source directory must not be a symlink: {}",
            directory.display()
        );
        directories += 1;
        anyhow::ensure!(
            directories <= maximum,
            "file index sweep exceeds {maximum} directories"
        );
        for entry in std::fs::read_dir(&directory)
            .with_context(|| format!("walk {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if excluded(&path) {
                continue;
            }
            let metadata = entry.file_type()?;
            if metadata.is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                anyhow::ensure!(
                    result.len() < maximum,
                    "file index sweep exceeds {maximum} files"
                );
                result.push(path);
            }
        }
        anyhow::ensure!(
            pending.len() <= maximum,
            "file index directory queue exceeds its bound"
        );
    }
    result.sort();
    result.dedup();
    Ok(result)
}
