//! Disk layout + IO primitives for durable sessions.
//!
//! This module is the **only** place that knows where session files
//! live on disk. Every other layer (`cos-apid`, agent runtimes, future
//! GUI) goes through these functions so we can rearrange the layout
//! later without touching callers.
//!
//! ## Atomicity story
//!
//! - Small JSON files (`meta.json`, `caps.json`, `state.json`,
//!   `lease.json`): written via [`crate::filelock::write_locked`] —
//!   write-tmp + atomic rename, under an exclusive `flock`.
//! - JSONL append-only logs (`turns.jsonl`, `mutations.jsonl`): written
//!   via [`crate::filelock::append_locked`] — `O_APPEND` + exclusive
//!   `flock` per line.
//! - The session directory itself is created with [`fs::create_dir_all`]
//!   and the per-file IO does the rest.
//!
//! ## Crash behavior
//!
//! - Mid-write crash on a small JSON: the previous on-disk file is
//!   intact (rename only happens after the tmp is fully written).
//! - Mid-write crash on a JSONL: the partial last line is invalid; the
//!   iter functions tolerate it by skipping lines that fail to parse,
//!   so the rest of the file is recoverable.
//!
//! ## What this module **does not** do (yet)
//!
//! - It does not enforce a [`Lease`] — `record_mutation` / `append_turn`
//!   accept any sid that exists on disk. Phase 2 adds lease guards.
//! - It does not GC archived sessions — Phase 1.5.
//! - It does not call `caps::require` on its own helpers; the gate
//!   lives at the api / CLI boundary that wraps these helpers, which
//!   matches how `audit.rs` and `trace.rs` are structured today.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::caps::CapSet;

use crate::filelock;
use crate::paths;

use super::id::SessionId;
use super::meta::{Lease, SessionMeta};
use super::mutation::MutationRecord;
use super::turn::Turn;

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Root directory holding every session subdir. Resolves via
/// [`crate::paths::data_dir`] so `COS_DATA_DIR` overrides apply.
pub fn sessions_root() -> PathBuf {
    paths::data_dir().join("sessions")
}

/// Directory for a single session. Does not check whether the dir
/// exists; callers wanting that should use [`get_meta`].
pub fn session_dir(sid: &SessionId) -> PathBuf {
    sessions_root().join(sid.as_str())
}

fn meta_path(sid: &SessionId) -> PathBuf {
    session_dir(sid).join("meta.json")
}

fn caps_path(sid: &SessionId) -> PathBuf {
    session_dir(sid).join("caps.json")
}

fn turns_path(sid: &SessionId) -> PathBuf {
    session_dir(sid).join("turns.jsonl")
}

fn mutations_path(sid: &SessionId) -> PathBuf {
    session_dir(sid).join("mutations.jsonl")
}

fn state_path(sid: &SessionId) -> PathBuf {
    session_dir(sid).join("state.json")
}

pub(super) fn lease_path(sid: &SessionId) -> PathBuf {
    session_dir(sid).join("lease.json")
}

pub(super) fn lease_lock_path(sid: &SessionId) -> PathBuf {
    session_dir(sid).join("lease.lock")
}

fn files_dir(sid: &SessionId) -> PathBuf {
    session_dir(sid).join("files")
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Everything `store` can return. Stringly-typed at the boundary
/// because every caller serializes errors back to JSON for the agent /
/// CLI, matching `filelock` and `proc`.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("session not found: {0}")]
    NotFound(String),

    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("filelock: {0}")]
    Lock(String),

    #[error("decode {path}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
}

impl SessionError {
    pub(super) fn io(path: PathBuf, source: std::io::Error) -> Self {
        Self::Io { path, source }
    }
    fn decode(path: PathBuf, source: serde_json::Error) -> Self {
        Self::Decode { path, source }
    }
}

type Result<T> = std::result::Result<T, SessionError>;

// ---------------------------------------------------------------------------
// Create / list / end
// ---------------------------------------------------------------------------

/// Materialize a new session on disk. Returns the freshly-minted id.
///
/// Writes `meta.json`, an empty `caps.json`, and creates the empty
/// `files/` directory so callers don't have to mkdir-race when writing
/// the first scratch artifact.
///
/// The caller may pass a `purpose` — a free-form label such as the
/// user's prompt, a workflow name, or an app manifest summary. Empty
/// strings are allowed (some api callers won't know the purpose until
/// the first turn arrives).
pub fn create(purpose: impl Into<String>) -> Result<SessionId> {
    let id = SessionId::generate();
    let dir = session_dir(&id);
    fs::create_dir_all(&dir).map_err(|e| SessionError::io(dir.clone(), e))?;
    fs::create_dir_all(files_dir(&id)).map_err(|e| SessionError::io(files_dir(&id), e))?;

    let meta = SessionMeta::fresh(id.clone(), purpose);
    write_json(&meta_path(&id), &meta)?;
    write_json(&caps_path(&id), &CapSet::new())?;
    Ok(id)
}

/// All session metadata currently on disk, in arbitrary order. Skips
/// directories whose `meta.json` is missing or corrupt (so a half-
/// created or hand-edited session doesn't take down `cos agent ls`).
///
/// The store does no caching — every call is a fresh disk walk. With
/// the typical few-hundred-sessions ceiling per machine this is fine;
/// if it ever isn't, the api socket is the place to add an index.
pub fn list() -> Result<Vec<SessionMeta>> {
    let root = sessions_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries = fs::read_dir(&root).map_err(|e| SessionError::io(root.clone(), e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(sid) = name.parse::<SessionId>() else {
            // Anything that isn't a canonical sid (stray file, archive
            // dir, dot-prefixed scratch) is skipped silently.
            continue;
        };
        match get_meta(&sid) {
            Ok(m) => out.push(m),
            Err(SessionError::NotFound(_)) => {} // dir exists but meta.json missing
            Err(SessionError::Decode { .. }) => {} // half-written / hand-edited
            Err(other) => return Err(other),
        }
    }
    Ok(out)
}

/// Mark the session terminal. Sets `status` and `ended_at`, then
/// rewrites `meta.json`. No-op if the session is already terminal.
pub fn end(sid: &SessionId, status: super::meta::Status) -> Result<()> {
    debug_assert!(
        !status.is_active(),
        "session::end requires a terminal status (Done | Failed)"
    );
    update_meta(sid, |m| {
        if m.status.is_active() {
            m.status = status;
            m.ended_at = Some(super::meta::now_rfc3339());
        }
    })
}

// ---------------------------------------------------------------------------
// Meta accessors
// ---------------------------------------------------------------------------

pub fn get_meta(sid: &SessionId) -> Result<SessionMeta> {
    read_json(&meta_path(sid))
}

/// Read-modify-write a session's meta under a single exclusive
/// lock. Pre-fix this function did `get_meta` (shared lock,
/// dropped) followed by `write_json` (exclusive lock, fresh
/// acquisition). The window between drop and re-acquire allowed
/// two concurrent updaters to both read the same value and both
/// write — the last writer's update silently overwrote the first.
/// `filelock::update_locked` holds the lock for the entire RMW.
pub fn update_meta<F: FnOnce(&mut SessionMeta)>(sid: &SessionId, f: F) -> Result<()> {
    let path = meta_path(sid);
    let mut closure = Some(f);
    filelock::update_locked(&path, |current| {
        let current = current.ok_or_else(|| SessionError::NotFound(sid.to_string()))?;
        let mut meta: SessionMeta = serde_json::from_str(&current)
            .map_err(|e| SessionError::decode(path.clone(), e))?;
        let f = closure.take().expect("closure called at most once");
        f(&mut meta);
        serde_json::to_string_pretty(&meta).map_err(SessionError::Encode)
    })
    .map_err(|e| match e {
        filelock::UpdateLockError::Io(msg) => SessionError::Lock(msg),
        filelock::UpdateLockError::Transform(inner) => inner,
    })
}

// ---------------------------------------------------------------------------
// Caps accessors
// ---------------------------------------------------------------------------

pub fn get_caps(sid: &SessionId) -> Result<CapSet> {
    // `caps.json` may not exist if a writer crashed between mkdir
    // and the first write. Treat missing-but-dir-exists as "empty
    // CapSet" so callers don't have to special-case it.
    let path = caps_path(sid);
    if !path.exists() {
        if session_dir(sid).exists() {
            return Ok(CapSet::new());
        }
        return Err(SessionError::NotFound(sid.to_string()));
    }
    read_json(&path)
}

pub fn set_caps(sid: &SessionId, caps: &CapSet) -> Result<()> {
    if !session_dir(sid).exists() {
        return Err(SessionError::NotFound(sid.to_string()));
    }
    write_json(&caps_path(sid), caps)
}

// ---------------------------------------------------------------------------
// State (per-runtime opaque scratch)
// ---------------------------------------------------------------------------

/// Read the entire `state.json` (the `{ "<runtime_id>": <value> }`
/// map). Returns the value at `runtime` if present, or `Value::Null`.
pub fn read_state(sid: &SessionId, runtime: &str) -> Result<Value> {
    let path = state_path(sid);
    if !path.exists() {
        if session_dir(sid).exists() {
            return Ok(Value::Null);
        }
        return Err(SessionError::NotFound(sid.to_string()));
    }
    let all: Value = read_json(&path)?;
    Ok(all.get(runtime).cloned().unwrap_or(Value::Null))
}

/// Write `value` at key `runtime` in `state.json`, preserving other
/// runtimes' entries. Read-modify-write under a single exclusive
/// lock so concurrent writers for different runtimes don't lose
/// each other's entries.
pub fn write_state(sid: &SessionId, runtime: &str, value: Value) -> Result<()> {
    if !session_dir(sid).exists() {
        return Err(SessionError::NotFound(sid.to_string()));
    }
    let path = state_path(sid);
    let runtime = runtime.to_string();
    let value_holder = std::cell::RefCell::new(Some(value));
    filelock::update_locked(&path, |current| {
        let mut all: Value = match current {
            Some(text) if !text.is_empty() => serde_json::from_str(&text)
                .map_err(|e| SessionError::decode(path.clone(), e))?,
            _ => Value::Object(serde_json::Map::new()),
        };
        if !all.is_object() {
            // Recover from a malformed prior write rather than
            // overwrite other runtimes' (now-unknown) entries.
            all = Value::Object(serde_json::Map::new());
        }
        let obj = all.as_object_mut().expect("ensured object above");
        let v = value_holder
            .borrow_mut()
            .take()
            .expect("transform called at most once");
        if v.is_null() {
            obj.remove(&runtime);
        } else {
            obj.insert(runtime.clone(), v);
        }
        serde_json::to_string_pretty(&all).map_err(SessionError::Encode)
    })
    .map_err(|e| match e {
        filelock::UpdateLockError::Io(msg) => SessionError::Lock(msg),
        filelock::UpdateLockError::Transform(inner) => inner,
    })
}

// ---------------------------------------------------------------------------
// Turns
// ---------------------------------------------------------------------------

/// Append a turn to `turns.jsonl`. Assigns a monotonic `seq` by
/// counting the lines already in the file **under the same exclusive
/// flock that performs the append**, so concurrent writers cannot
/// collide on the same seq. Stamps `at` with the current time if the
/// caller left it empty.
///
/// Returns the seq actually written.
pub fn append_turn(sid: &SessionId, mut turn: Turn) -> Result<u64> {
    if !session_dir(sid).exists() {
        return Err(SessionError::NotFound(sid.to_string()));
    }
    append_jsonl_with_seq(&turns_path(sid), |seq| {
        turn.seq = seq;
        turn.stamp_default_time();
        serde_json::to_string(&turn)
    })
}

/// Iterate every turn in order. Tolerates a trailing partial line.
pub fn iter_turns(sid: &SessionId) -> Result<Vec<Turn>> {
    read_jsonl(&turns_path(sid), session_dir(sid))
}

// ---------------------------------------------------------------------------
// Mutations
// ---------------------------------------------------------------------------

/// Append a mutation to `mutations.jsonl`. Same `seq` / `at` semantics
/// as [`append_turn`], including the count-and-append-under-one-flock
/// concurrency guarantee.
pub fn record_mutation(sid: &SessionId, mut rec: MutationRecord) -> Result<u64> {
    if !session_dir(sid).exists() {
        return Err(SessionError::NotFound(sid.to_string()));
    }
    append_jsonl_with_seq(&mutations_path(sid), |seq| {
        rec.seq = seq;
        rec.stamp_default_time();
        serde_json::to_string(&rec)
    })
}

pub fn iter_mutations(sid: &SessionId) -> Result<Vec<MutationRecord>> {
    read_jsonl(&mutations_path(sid), session_dir(sid))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn write_json<T: serde::Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let data = serde_json::to_string_pretty(value)?;
    filelock::write_locked(path, &data).map_err(SessionError::Lock)
}

fn read_json<T: DeserializeOwned>(path: &PathBuf) -> Result<T> {
    let raw = filelock::read_locked(path).map_err(SessionError::Lock)?;
    let Some(text) = raw else {
        return Err(SessionError::NotFound(
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string(),
        ));
    };
    serde_json::from_str(&text).map_err(|e| SessionError::decode(path.clone(), e))
}

/// Count + append a JSONL line under a single exclusive `flock`.
///
/// The `build` closure receives the assigned seq (one greater than
/// the count of non-empty lines currently in the file) and returns
/// the serialized line to write. Doing count + write under one lock
/// is **the** correctness property of this module: it prevents two
/// concurrent `append_turn` calls from minting the same seq.
///
/// On non-Unix platforms (no `flock`) the function still works but
/// without true mutual exclusion; this is the same trade-off
/// [`crate::filelock`] makes for non-Linux builds.
fn append_jsonl_with_seq<F>(path: &PathBuf, build: F) -> Result<u64>
where
    F: FnOnce(u64) -> std::result::Result<String, serde_json::Error>,
{
    use std::io::{Seek, SeekFrom, Write};

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| SessionError::io(parent.to_path_buf(), e))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|e| SessionError::io(path.clone(), e))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(SessionError::Lock(format!(
                "flock LOCK_EX {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
    }

    // Count non-empty lines while we hold the exclusive lock.
    file.seek(SeekFrom::Start(0))
        .map_err(|e| SessionError::io(path.clone(), e))?;
    let mut reader = BufReader::new(&file);
    let mut buf = String::new();
    let mut count: u64 = 0;
    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                if !buf.trim_end().is_empty() {
                    count += 1;
                }
            }
            Err(e) => return Err(SessionError::io(path.clone(), e)),
        }
    }
    drop(reader);

    let line = build(count).map_err(SessionError::Encode)?;

    // File was opened with O_APPEND, so the write always lands at
    // the current end-of-file regardless of where we seeked above.
    writeln!(&mut file, "{}", line).map_err(|e| SessionError::io(path.clone(), e))?;
    file.flush().map_err(|e| SessionError::io(path.clone(), e))?;
    // Persist the new line to disk *before* releasing the lock.
    // Without `sync_data()` a `cos agent undo` started by the next
    // process can read a turn from the page cache that the kernel
    // hasn't yet committed; a crash after the next process exits
    // would then lose the entry our caller already considers
    // persisted. `sync_data` is cheaper than `sync_all` and enough
    // here because we don't care about journaling the directory
    // entry — the file already exists.
    file.sync_data()
        .map_err(|e| SessionError::io(path.clone(), e))?;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
    drop(file);
    Ok(count)
}

/// Read a JSONL log into a Vec.
///
/// IO errors *are* propagated — earlier revisions silently dropped
/// them via `map_while(Result::ok)`, which masked truncation and
/// partial-write recovery problems. Lines that parse as JSON but
/// don't deserialize to `T` are still skipped (best-effort recovery
/// from a crash mid-line is by design).
///
/// `session_dir` is passed in so we can raise `NotFound` for a
/// missing session (vs. an empty log inside a real session).
fn read_jsonl<T: DeserializeOwned>(path: &PathBuf, dir: PathBuf) -> Result<Vec<T>> {
    if !path.exists() {
        if dir.exists() {
            return Ok(Vec::new());
        }
        return Err(SessionError::NotFound(
            dir.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string(),
        ));
    }
    let file = fs::File::open(path).map_err(|e| SessionError::io(path.clone(), e))?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(|e| SessionError::io(path.clone(), e))?;
        if line.is_empty() {
            continue;
        }
        // A crash mid-write can leave one trailing partial line.
        // Skip lines that fail to deserialize — readers want
        // best-effort recovery — but do **not** swallow the IO
        // error that produced an empty line in the first place.
        if let Ok(v) = serde_json::from_str::<T>(&line) {
            out.push(v);
        }
    }
    Ok(out)
}

// `Lease` IO is owned by `session::lease` (Phase 2). This module
// exposes the disk primitives — atomic JSON read/write — that the
// lease module composes with `flock` on the sentinel `lease.lock`
// file to implement cross-process acquire / release / heartbeat.
pub(super) fn write_lease(sid: &SessionId, lease: &Lease) -> Result<()> {
    write_json(&lease_path(sid), lease)
}

pub(super) fn read_lease(sid: &SessionId) -> Result<Option<Lease>> {
    let path = lease_path(sid);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(read_json(&path)?))
}

pub(super) fn remove_lease(sid: &SessionId) -> Result<()> {
    let path = lease_path(sid);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SessionError::io(path, e)),
    }
}
