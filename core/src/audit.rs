use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};

const CHAIN_VERSION: u64 = 1;
const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const MAX_AUDIT_LINE_BYTES: usize = 1024 * 1024;
const MAX_ARCHIVE_DEPTH: usize = 64;

/// Redact sensitive patterns from args before logging.
/// Catches bearer/token prefixes, common API key prefixes, URL
/// userinfo segments, and authorization headers (both as their own
/// arg and as a substring of a larger arg like `--header
/// "Authorization: Bearer ..."` or `-H Authorization: ...`).
fn redact_args(args: &[String]) -> Vec<String> {
    args.iter().map(|arg| redact_one(arg)).collect()
}

fn wholly_legacy_log(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut found = false;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            return false;
        };
        if !value.is_object() || has_any_chain_field(&value) {
            return false;
        }
        found = true;
    }
    found
}

fn nonempty_line_count(bytes: &[u8]) -> usize {
    std::str::from_utf8(bytes)
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0)
}

fn redact_one(arg: &str) -> String {
    let lower = arg.to_lowercase();
    // Whole-arg auth tokens
    if lower.starts_with("bearer ") || lower.starts_with("token ") {
        return "***REDACTED***".to_string();
    }
    // Whole-arg API key shapes
    if arg.starts_with("sk-")
        || arg.starts_with("ghp_")
        || arg.starts_with("ghs_")
        || arg.starts_with("gho_")
        || arg.starts_with("ghu_")
        || arg.starts_with("ghr_")
        || arg.starts_with("glpat-")
        || arg.starts_with("xoxb-")
        || arg.starts_with("xoxp-")
        || arg.starts_with("xoxa-")
        || arg.starts_with("xoxs-")
        || arg.starts_with("AKIA")
    {
        return "***REDACTED***".to_string();
    }
    // Authorization header — may be a whole arg ("Authorization: Bearer X")
    // or embedded inside an arg ('--header Authorization: ...').
    if let Some(idx) = lower.find("authorization:") {
        let prefix = &arg[..idx];
        return format!("{prefix}Authorization: ***REDACTED***");
    }
    // URLs with embedded credentials (https://user:pass@host).
    // Replace `user:pass@` with `***REDACTED***@` while keeping the
    // host/path visible for triage.
    if let Some(redacted) = redact_url_creds(arg) {
        return redacted;
    }
    arg.to_string()
}

/// If `arg` contains a `://user:pass@` userinfo segment, return a
/// redacted copy. Returns None when there's nothing credential-like
/// to redact.
fn redact_url_creds(arg: &str) -> Option<String> {
    let scheme_end = arg.find("://")?;
    let after_scheme = &arg[scheme_end + 3..];
    // Userinfo ends at the next '@' that comes before the path / query.
    let at_idx = after_scheme.find('@')?;
    let userinfo = &after_scheme[..at_idx];
    if !userinfo.contains(':') {
        // username-only (e.g. github.com/user@email/repo paths) — be
        // conservative and only redact when it looks like user:pass.
        return None;
    }
    // Stop scanning at the first path/query/fragment delimiter to
    // avoid pulling in '@' from inside a path.
    let stop = userinfo.find(['/', '?', '#', ' ']).unwrap_or(usize::MAX);
    if stop != usize::MAX {
        return None;
    }
    let mut out = String::with_capacity(arg.len());
    out.push_str(&arg[..scheme_end + 3]);
    out.push_str("***REDACTED***@");
    out.push_str(&after_scheme[at_idx + 1..]);
    Some(out)
}

/// Write an audit log entry to the JSONL file.
pub fn log_entry(
    audit_path: &Path,
    app: &str,
    command: &str,
    args: &[String],
    start: Instant,
    status: &str,
    error: Option<&str>,
) {
    let duration_ms = start.elapsed().as_millis() as u64;
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let safe_args = redact_args(args);

    let mut entry = json!({
        "timestamp": timestamp,
        "app": app,
        "command": command,
        "args": safe_args,
        "duration_ms": duration_ms,
        "status": status,
    });

    if let Some(e) = error {
        entry["error"] = json!(e);
    }

    // Attach trace context if available
    if let Ok(trace_id) = std::env::var("COS_TRACE_ID") {
        if !trace_id.is_empty() {
            entry["trace_id"] = json!(trace_id);
        }
    }
    if let Ok(span_id) = std::env::var("COS_SPAN_ID") {
        if !span_id.is_empty() {
            entry["span_id"] = json!(span_id);
        }
    }

    if let Some(parent) = audit_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let _ = crate::filelock::append_locked(audit_path, &entry.to_string());
}

/// Append a structured JSONL audit event with arbitrary shape.
///
/// Used by callers that need a richer schema than the
/// `app/command/args/duration_ms/status` shape produced by
/// [`log_entry`] — for example, the agent runtime's `AuditHook`
/// emits `{ kind, session_id, turn, tool_name, latency_ms,
/// bytes_returned, error }`.
///
/// Behaviour:
///   - `timestamp` is auto-injected (UTC, `YYYY-MM-DDTHH:MM:SSZ`)
///     if the entry doesn't already have one.
///   - `trace_id` / `span_id` are injected from `COS_TRACE_ID` /
///     `COS_SPAN_ID` env vars when set and not already present.
///   - Parent directory of `audit_path` is created if missing.
///   - Write is appended atomically via the file-lock helper.
///
/// This lightweight writer is used by high-frequency logs such as capability
/// decisions. It does not fsync or hash-chain each line.
pub fn log_event(audit_path: &Path, mut entry: serde_json::Value) {
    inject_standard_fields(&mut entry);
    if let Some(parent) = audit_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Err(error) = crate::filelock::append_locked(audit_path, &entry.to_string()) {
        tracing::warn!(
            path = %audit_path.display(),
            %error,
            "failed to append audit event"
        );
    }
}

/// Append one tamper-evident structured event.
///
/// Events are hash-chained under the stable path lock. Existing legacy logs
/// are copied to a hash-anchored archive before the first chained event.
pub fn log_chained_event(audit_path: &Path, mut entry: serde_json::Value) {
    inject_standard_fields(&mut entry);
    if let Err(error) = append_chained_event(audit_path, entry) {
        tracing::warn!(
            path = %audit_path.display(),
            %error,
            "failed to append hash-chained audit event"
        );
    }
}

pub fn verify_hash_chain(path: &Path) -> Result<serde_json::Value, String> {
    if !path.exists() && !chain_head_path(path).exists() {
        return Ok(ChainVerification {
            valid: true,
            ..Default::default()
        }
        .into_json(path));
    }
    crate::filelock::with_exclusive_path_lock(path, || {
        let root = path
            .parent()
            .ok_or_else(|| "audit path has no parent".to_string())?;
        let mut visited = BTreeSet::new();
        let report = verify_path(path, root, 0, &mut visited)?;
        Ok(report.into_json(path))
    })
}

pub fn hash_chain_storage_bytes(path: &Path) -> Result<u64, String> {
    if !path.exists() {
        return Ok(0);
    }
    crate::filelock::with_exclusive_path_lock(path, || {
        let root = path
            .parent()
            .ok_or_else(|| "audit path has no parent".to_string())?;
        let mut visited = BTreeSet::new();
        estimate_chain_bytes(path, root, 0, &mut visited)
    })
}

pub fn archive_hash_chain(path: &Path) -> Result<serde_json::Value, String> {
    archive_hash_chain_inner(path, false)
}

pub fn quarantine_hash_chain(path: &Path) -> Result<serde_json::Value, String> {
    archive_hash_chain_inner(path, true)
}

fn archive_hash_chain_inner(
    path: &Path,
    acknowledge_invalid: bool,
) -> Result<serde_json::Value, String> {
    crate::filelock::with_exclusive_path_lock(path, || {
        if !path.exists() {
            return Ok(json!({
                "path": path.display().to_string(),
                "cleared": false,
                "archived": false,
                "reason": "file does not exist",
            }));
        }
        let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
        if bytes.is_empty() {
            fs::remove_file(path)
                .map_err(|error| format!("remove empty {}: {error}", path.display()))?;
            let head_path = chain_head_path(path);
            if head_path.exists() {
                fs::remove_file(&head_path)
                    .map_err(|error| format!("remove stale {}: {error}", head_path.display()))?;
            }
            return Ok(json!({
                "path": path.display().to_string(),
                "cleared": true,
                "archived": false,
                "reason": "empty log removed",
            }));
        }

        let verification = verify_chain_bytes(&bytes);
        let legacy = wholly_legacy_log(&bytes);
        if acknowledge_invalid {
            let root = path
                .parent()
                .ok_or_else(|| "audit path has no parent".to_string())?;
            let mut visited = BTreeSet::new();
            let full = verify_path(path, root, 0, &mut visited)?;
            if full.valid || full.legacy {
                return Err(
                    "quarantine is only for an invalid chained log; use clear for valid or legacy logs"
                        .to_string(),
                );
            }
        }
        let archive_kind = if verification.valid {
            "chain"
        } else if legacy {
            "legacy"
        } else if acknowledge_invalid {
            "quarantined-invalid"
        } else {
            "invalid-chain"
        };
        let archive = copy_to_archive(path, &bytes, "archive")?;
        let mut anchor = json!({
            "kind": "audit_archive",
            "archived_at": Utc::now().to_rfc3339(),
            "previous_archive": archive.relative_path.clone(),
            "previous_archive_sha256": archive.sha256.clone(),
            "previous_archive_kind": archive_kind,
            "previous_chain_valid": if archive_kind == "legacy" { serde_json::Value::Null } else { json!(verification.valid) },
            "previous_chain_last_hash": verification.last_hash,
            "previous_chain_events": verification.events,
            "previous_chain_errors": verification.errors,
        });
        inject_standard_fields(&mut anchor);
        write_first_event_locked(path, anchor)
            .map_err(|error| format!("start new audit chain after archive failed: {error}"))?;
        Ok(json!({
            "path": path.display().to_string(),
            "cleared": true,
            "archived": true,
            "archive_path": archive.path.display().to_string(),
            "archive_sha256": archive.sha256,
            "previous_chain_valid": if archive_kind == "legacy" { serde_json::Value::Null } else { json!(verification.valid) },
            "new_chain_started": true,
            "quarantined": acknowledge_invalid,
        }))
    })
}

fn append_chained_event(path: &Path, mut entry: serde_json::Value) -> Result<(), String> {
    crate::filelock::with_exclusive_path_lock(path, || {
        let mut chain_id = uuid::Uuid::new_v4().simple().to_string();
        let mut sequence = 1_u64;
        let mut previous_hash = GENESIS_HASH.to_string();

        if path.exists() {
            let mut file = OpenOptions::new()
                .read(true)
                .open(path)
                .map_err(|error| format!("open {}: {error}", path.display()))?;
            if let Some(last_line) = read_last_nonempty_line(&mut file)? {
                let last: serde_json::Value = match serde_json::from_str(&last_line) {
                    Ok(last) => last,
                    Err(error) => {
                        return recover_invalid_tail(
                            path,
                            entry,
                            format!("malformed audit tail: {error}"),
                            true,
                        )
                    }
                };
                if has_any_chain_field(&last) {
                    let tip = match parse_chain_tip(&last) {
                        Ok(tip) => tip,
                        Err(error) => return recover_invalid_tail(path, entry, error, false),
                    };
                    chain_id = tip.chain_id;
                    sequence = tip
                        .sequence
                        .checked_add(1)
                        .ok_or_else(|| "audit sequence exhausted u64".to_string())?;
                    previous_hash = tip.this_hash;
                } else {
                    let bytes = fs::read(path)
                        .map_err(|error| format!("read legacy {}: {error}", path.display()))?;
                    let archive = copy_to_archive(path, &bytes, "legacy")?;
                    ensure_object(&mut entry)?
                        .insert("previous_archive".to_string(), json!(archive.relative_path));
                    ensure_object(&mut entry)?
                        .insert("previous_archive_sha256".to_string(), json!(archive.sha256));
                    ensure_object(&mut entry)?
                        .insert("previous_archive_kind".to_string(), json!("legacy"));
                    ensure_object(&mut entry)?
                        .insert("previous_archive_bytes".to_string(), json!(archive.bytes));
                    return write_first_event_locked(path, entry);
                }
            }
        }

        write_chained_line(path, entry, &chain_id, sequence, &previous_hash)
    })
}

fn recover_invalid_tail(
    path: &Path,
    mut entry: serde_json::Value,
    error: String,
    allow_torn_tail: bool,
) -> Result<(), String> {
    let bytes =
        fs::read(path).map_err(|read_error| format!("read {}: {read_error}", path.display()))?;
    let torn_tail = allow_torn_tail && has_valid_prefix_and_head(path, &bytes);
    let archive_kind = if torn_tail {
        "torn-tail"
    } else {
        "invalid-chain"
    };
    let archive = copy_to_archive(path, &bytes, if torn_tail { "torn" } else { "invalid" })?;
    let object = ensure_object(&mut entry)?;
    object.insert("previous_archive".to_string(), json!(archive.relative_path));
    object.insert("previous_archive_sha256".to_string(), json!(archive.sha256));
    object.insert("previous_archive_kind".to_string(), json!(archive_kind));
    object.insert("previous_chain_valid".to_string(), json!(torn_tail));
    object.insert("previous_chain_error".to_string(), json!(error));
    write_first_event_locked(path, entry)
}

fn has_valid_prefix_and_head(path: &Path, bytes: &[u8]) -> bool {
    let Some(prefix) = prefix_before_last_nonempty_line(bytes) else {
        return false;
    };
    let mut report = verify_chain_bytes(prefix);
    if !report.valid || report.events == 0 {
        return false;
    }
    verify_chain_head(path, &mut report);
    report.valid
}

fn prefix_before_last_nonempty_line(bytes: &[u8]) -> Option<&[u8]> {
    let mut end = bytes.len();
    while end > 0 && matches!(bytes[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let start = bytes[..end]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)?;
    (start > 0).then_some(&bytes[..start])
}

fn write_first_event_locked(path: &Path, entry: serde_json::Value) -> Result<(), String> {
    let chain_id = uuid::Uuid::new_v4().simple().to_string();
    let (line, this_hash) = prepare_chained_line(entry, &chain_id, 1, GENESIS_HASH)?;
    let parent = path
        .parent()
        .ok_or_else(|| "audit path has no parent".to_string())?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("audit.jsonl");
    let tmp = parent.join(format!(
        ".{name}.{}.rotate.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true).mode(0o600);
    let mut file = options
        .open(&tmp)
        .map_err(|error| format!("open {}: {error}", tmp.display()))?;
    if let Err(error) = writeln!(file, "{line}").and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("write {}: {error}", tmp.display()));
    }
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("replace {}: {error}", path.display()));
    }
    crate::storage::set_private_file(path)
        .map_err(|error| format!("secure {}: {error}", path.display()))?;
    let _ = fs::File::open(parent).and_then(|directory| directory.sync_all());
    write_chain_head(path, &chain_id, 1, &this_hash)
}

fn write_chained_line(
    path: &Path,
    entry: serde_json::Value,
    chain_id: &str,
    sequence: u64,
    previous_hash: &str,
) -> Result<(), String> {
    let (line, this_hash) = prepare_chained_line(entry, chain_id, sequence, previous_hash)?;
    let mut options = OpenOptions::new();
    options.create(true).append(true).write(true).mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    let original_len = file
        .metadata()
        .map_err(|error| format!("inspect {}: {error}", path.display()))?
        .len();
    crate::storage::set_private_file(path)
        .map_err(|error| format!("secure {}: {error}", path.display()))?;
    if let Err(error) = writeln!(file, "{line}") {
        return Err(format!(
            "append {} failed ({error}); rollback: {}",
            path.display(),
            rollback_append(&mut file, original_len)
        ));
    }
    if let Err(error) = file.flush() {
        return Err(format!(
            "flush {} failed ({error}); rollback: {}",
            path.display(),
            rollback_append(&mut file, original_len)
        ));
    }
    if let Err(error) = file.sync_data() {
        return Err(format!(
            "sync {} failed ({error}); rollback: {}",
            path.display(),
            rollback_append(&mut file, original_len)
        ));
    }
    if let Err(error) = write_chain_head(path, chain_id, sequence, &this_hash) {
        return Err(format!(
            "persist audit chain head failed ({error}); append rollback: {}",
            rollback_append(&mut file, original_len)
        ));
    }

    fn rollback_append(file: &mut fs::File, original_len: u64) -> String {
        match file.set_len(original_len).and_then(|_| file.sync_data()) {
            Ok(()) => "ok".to_string(),
            Err(error) => error.to_string(),
        }
    }
    Ok(())
}

fn prepare_chained_line(
    mut entry: serde_json::Value,
    chain_id: &str,
    sequence: u64,
    previous_hash: &str,
) -> Result<(String, String), String> {
    let object = ensure_object(&mut entry)?;
    for field in [
        "chain_version",
        "chain_id",
        "sequence",
        "prev_hash",
        "this_hash",
    ] {
        object.remove(field);
    }
    object.insert("chain_version".to_string(), json!(CHAIN_VERSION));
    object.insert("chain_id".to_string(), json!(chain_id));
    object.insert("sequence".to_string(), json!(sequence));
    object.insert("prev_hash".to_string(), json!(previous_hash));
    let this_hash = event_hash(&entry)?;
    ensure_object(&mut entry)?.insert("this_hash".to_string(), json!(this_hash));
    let line =
        serde_json::to_string(&entry).map_err(|error| format!("serialize audit event: {error}"))?;
    if line.len() > MAX_AUDIT_LINE_BYTES {
        return Err(format!(
            "audit event exceeds {} bytes",
            MAX_AUDIT_LINE_BYTES
        ));
    }
    Ok((line, this_hash))
}

fn write_chain_head(
    path: &Path,
    chain_id: &str,
    sequence: u64,
    this_hash: &str,
) -> Result<(), String> {
    let head = json!({
        "chain_version": CHAIN_VERSION,
        "chain_id": chain_id,
        "sequence": sequence,
        "this_hash": this_hash,
        "updated_at": Utc::now().to_rfc3339(),
    });
    let data =
        serde_json::to_string(&head).map_err(|error| format!("serialize audit head: {error}"))?;
    crate::filelock::write_locked(&chain_head_path(path), &data)
}

fn chain_head_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".head");
    PathBuf::from(value)
}

fn inject_standard_fields(entry: &mut serde_json::Value) {
    if let Some(object) = entry.as_object_mut() {
        object
            .entry("timestamp")
            .or_insert_with(|| json!(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()));
        if !object.contains_key("trace_id") {
            if let Ok(trace_id) = std::env::var("COS_TRACE_ID") {
                if !trace_id.is_empty() {
                    object.insert("trace_id".to_string(), json!(trace_id));
                }
            }
        }
        if !object.contains_key("span_id") {
            if let Ok(span_id) = std::env::var("COS_SPAN_ID") {
                if !span_id.is_empty() {
                    object.insert("span_id".to_string(), json!(span_id));
                }
            }
        }
    }
}

fn ensure_object(
    value: &mut serde_json::Value,
) -> Result<&mut serde_json::Map<String, serde_json::Value>, String> {
    value
        .as_object_mut()
        .ok_or_else(|| "audit event must be a JSON object".to_string())
}

fn event_hash(event: &serde_json::Value) -> Result<String, String> {
    let mut unsigned = event.clone();
    ensure_object(&mut unsigned)?.remove("this_hash");
    let bytes = serde_json::to_vec(&unsigned)
        .map_err(|error| format!("serialize audit hash payload: {error}"))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn has_any_chain_field(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|object| {
        [
            "chain_version",
            "chain_id",
            "sequence",
            "prev_hash",
            "this_hash",
        ]
        .iter()
        .any(|field| object.contains_key(*field))
    })
}

struct ChainTip {
    chain_id: String,
    sequence: u64,
    this_hash: String,
}

fn parse_chain_tip(value: &serde_json::Value) -> Result<ChainTip, String> {
    let version = value
        .get("chain_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| "audit tail has incomplete chain_version".to_string())?;
    if version != CHAIN_VERSION {
        return Err(format!("unsupported audit chain version: {version}"));
    }
    let chain_id = value
        .get("chain_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| valid_token(value))
        .ok_or_else(|| "audit tail has invalid chain_id".to_string())?;
    let sequence = value
        .get("sequence")
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| "audit tail has invalid sequence".to_string())?;
    let previous_hash = value
        .get("prev_hash")
        .and_then(serde_json::Value::as_str)
        .filter(|value| valid_hash(value))
        .ok_or_else(|| "audit tail has invalid prev_hash".to_string())?;
    let this_hash = value
        .get("this_hash")
        .and_then(serde_json::Value::as_str)
        .filter(|value| valid_hash(value))
        .ok_or_else(|| "audit tail has invalid this_hash".to_string())?;
    let expected = event_hash(value)?;
    if this_hash != expected {
        return Err("audit tail hash does not match its contents".to_string());
    }
    if sequence == 1 && previous_hash != GENESIS_HASH {
        return Err("first audit event does not use the genesis hash".to_string());
    }
    Ok(ChainTip {
        chain_id: chain_id.to_string(),
        sequence,
        this_hash: this_hash.to_string(),
    })
}

fn read_last_nonempty_line(file: &mut fs::File) -> Result<Option<String>, String> {
    let length = file
        .seek(SeekFrom::End(0))
        .map_err(|error| format!("seek audit tail: {error}"))?;
    if length == 0 {
        return Ok(None);
    }
    let read_len = length.min((MAX_AUDIT_LINE_BYTES + 2) as u64) as usize;
    file.seek(SeekFrom::Start(length - read_len as u64))
        .map_err(|error| format!("seek audit tail window: {error}"))?;
    let mut buffer = vec![0_u8; read_len];
    file.read_exact(&mut buffer)
        .map_err(|error| format!("read audit tail window: {error}"))?;
    let mut end = buffer.len();
    while end > 0 && matches!(buffer[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    if end == 0 {
        if length > read_len as u64 {
            return Err("audit tail contains excessive blank padding".to_string());
        }
        return Ok(None);
    }
    let start = buffer[..end]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    if start == 0 && length > read_len as u64 {
        return Err(format!("audit tail exceeds {} bytes", MAX_AUDIT_LINE_BYTES));
    }
    String::from_utf8(buffer[start..end].to_vec())
        .map(Some)
        .map_err(|error| format!("audit tail is not UTF-8: {error}"))
}

struct ArchiveRecord {
    path: PathBuf,
    relative_path: String,
    sha256: String,
    bytes: usize,
}

fn copy_to_archive(path: &Path, bytes: &[u8], label: &str) -> Result<ArchiveRecord, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "audit path has no parent".to_string())?;
    let archive_dir = parent.join("archive");
    crate::storage::ensure_private_dir(&archive_dir)
        .map_err(|error| format!("secure {}: {error}", archive_dir.display()))?;
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("audit");
    let name = format!(
        "{}-{}-{}-{}.jsonl",
        stem,
        label,
        Utc::now().format("%Y%m%dT%H%M%S"),
        uuid::Uuid::new_v4().simple()
    );
    let archive_path = archive_dir.join(name);
    let mut options = OpenOptions::new();
    options.create_new(true).write(true).mode(0o600);
    let mut archive_file = options
        .open(&archive_path)
        .map_err(|error| format!("create {}: {error}", archive_path.display()))?;
    if let Err(error) = archive_file
        .write_all(bytes)
        .and_then(|_| archive_file.sync_all())
    {
        let _ = fs::remove_file(&archive_path);
        return Err(format!("write {}: {error}", archive_path.display()));
    }
    if let Err(error) = crate::storage::set_private_file(&archive_path) {
        let _ = fs::remove_file(&archive_path);
        return Err(format!("secure {}: {error}", archive_path.display()));
    }
    let _ = fs::File::open(&archive_dir).and_then(|directory| directory.sync_all());
    let _ = fs::File::open(parent).and_then(|directory| directory.sync_all());
    Ok(ArchiveRecord {
        relative_path: archive_path
            .strip_prefix(parent)
            .map_err(|_| "audit archive escaped its parent".to_string())?
            .to_string_lossy()
            .to_string(),
        path: archive_path,
        sha256: hex::encode(Sha256::digest(bytes)),
        bytes: bytes.len(),
    })
}

#[derive(Default)]
struct ChainVerification {
    valid: bool,
    legacy: bool,
    events: usize,
    chain_id: Option<String>,
    first_hash: Option<String>,
    last_hash: Option<String>,
    last_prev_hash: Option<String>,
    last_sequence: Option<u64>,
    errors: Vec<String>,
    warnings: Vec<String>,
    archives: Vec<serde_json::Value>,
}

impl ChainVerification {
    fn into_json(self, path: &Path) -> serde_json::Value {
        json!({
            "path": path.display().to_string(),
            "valid": self.valid,
            "status": if self.legacy { "legacy" } else if self.valid { "valid" } else { "invalid" },
            "legacy": self.legacy,
            "events": self.events,
            "chain_id": self.chain_id,
            "first_hash": self.first_hash,
            "last_hash": self.last_hash,
            "last_prev_hash": self.last_prev_hash,
            "last_sequence": self.last_sequence,
            "errors": self.errors,
            "warnings": self.warnings,
            "archives": self.archives,
        })
    }
}

fn verify_path(
    path: &Path,
    root: &Path,
    depth: usize,
    visited: &mut BTreeSet<PathBuf>,
) -> Result<ChainVerification, String> {
    if depth > MAX_ARCHIVE_DEPTH {
        return Err("audit archive chain exceeds maximum depth".to_string());
    }
    let canonical_key = path.to_path_buf();
    if !visited.insert(canonical_key) {
        return Err(format!(
            "audit archive cycle detected at {}",
            path.display()
        ));
    }
    if !path.exists() {
        let head_path = chain_head_path(path);
        if head_path.exists() && depth == 0 {
            return Ok(ChainVerification {
                valid: false,
                errors: vec![format!(
                    "audit log is missing but chain head exists: {}",
                    head_path.display()
                )],
                ..Default::default()
            });
        }
        return Ok(ChainVerification {
            valid: true,
            ..Default::default()
        });
    }
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    if wholly_legacy_log(&bytes) {
        let head_path = chain_head_path(path);
        let mut report = ChainVerification {
            valid: false,
            legacy: true,
            events: nonempty_line_count(&bytes),
            warnings: vec![
                "legacy audit log is not hash-chained yet; the next audit append will migrate it"
                    .to_string(),
            ],
            ..Default::default()
        };
        if head_path.exists() && depth == 0 {
            report.errors.push(format!(
                "legacy audit log unexpectedly has a chain head: {}",
                head_path.display()
            ));
            report.legacy = false;
        }
        return Ok(report);
    }
    let mut report = verify_chain_bytes(&bytes);
    if let Some(first) = first_event(&bytes) {
        let link = match archive_link(root, &first) {
            Ok(link) => link,
            Err(error) => {
                report.valid = false;
                report.errors.push(error);
                return Ok(report);
            }
        };
        if let Some(link) = link {
            let actual = match fs::read(&link.path) {
                Ok(actual) => actual,
                Err(error) => {
                    report.valid = false;
                    report.errors.push(format!(
                        "read linked archive {}: {error}",
                        link.path.display()
                    ));
                    return Ok(report);
                }
            };
            let actual_hash = hex::encode(Sha256::digest(&actual));
            let hash_valid = actual_hash == link.sha256;
            let mut archive_json = json!({
                "path": link.path.display().to_string(),
                "kind": link.kind,
                "sha256": link.sha256,
                "hash_valid": hash_valid,
            });
            if !hash_valid {
                report.valid = false;
                report.errors.push(format!(
                    "linked archive hash mismatch: {}",
                    link.path.display()
                ));
            } else if link.kind == "chain" {
                let nested = match verify_path(&link.path, root, depth + 1, visited) {
                    Ok(nested) => nested,
                    Err(error) => {
                        report.valid = false;
                        report.errors.push(error);
                        report.archives.push(archive_json);
                        return Ok(report);
                    }
                };
                if !nested.valid {
                    report.valid = false;
                    report.errors.push(format!(
                        "linked archive chain is invalid: {}",
                        link.path.display()
                    ));
                }
                for warning in &nested.warnings {
                    report
                        .warnings
                        .push(format!("linked archive {}: {warning}", link.path.display()));
                }
                archive_json["chain"] = nested.into_json(&link.path);
            } else if link.kind == "legacy" {
                report.warnings.push(format!(
                    "legacy archive is hash-anchored but was not internally chained: {}",
                    link.path.display()
                ));
            } else if link.kind == "torn-tail" {
                report.warnings.push(format!(
                    "archive contains a recoverable torn final line and is hash-anchored: {}",
                    link.path.display()
                ));
            } else if link.kind == "quarantined-invalid" {
                report.warnings.push(format!(
                    "operator quarantined an invalid historical chain; archive remains hash-anchored: {}",
                    link.path.display()
                ));
            } else {
                report.valid = false;
                report.errors.push(format!(
                    "linked archive was already marked invalid: {}",
                    link.path.display()
                ));
            }
            report.archives.push(archive_json);
        }
    }
    if depth == 0 {
        verify_chain_head(path, &mut report);
    }
    Ok(report)
}

fn verify_chain_bytes(bytes: &[u8]) -> ChainVerification {
    let mut report = ChainVerification {
        valid: true,
        ..Default::default()
    };
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            report.valid = false;
            report
                .errors
                .push(format!("audit log is not UTF-8: {error}"));
            return report;
        }
    };
    let mut expected_previous = GENESIS_HASH.to_string();
    let mut expected_sequence = 1_u64;
    let mut chain_id: Option<String> = None;
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        report.events += 1;
        let event: serde_json::Value = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(event) if event.is_object() => event,
            Ok(_) => {
                report.valid = false;
                report
                    .errors
                    .push(format!("line {} is not a JSON object", line_index + 1));
                continue;
            }
            Err(error) => {
                report.valid = false;
                report.errors.push(format!(
                    "line {} is malformed JSON: {error}",
                    line_index + 1
                ));
                continue;
            }
        };
        let tip = match parse_chain_tip(&event) {
            Ok(tip) => tip,
            Err(error) => {
                report.valid = false;
                report
                    .errors
                    .push(format!("line {}: {error}", line_index + 1));
                continue;
            }
        };
        let event_previous = event
            .get("prev_hash")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if tip.sequence != expected_sequence {
            report.valid = false;
            report.errors.push(format!(
                "line {} sequence {}, expected {}",
                line_index + 1,
                tip.sequence,
                expected_sequence
            ));
        }
        if event_previous != expected_previous {
            report.valid = false;
            report
                .errors
                .push(format!("line {} prev_hash mismatch", line_index + 1));
        }
        match chain_id.as_deref() {
            Some(expected) if expected != tip.chain_id => {
                report.valid = false;
                report
                    .errors
                    .push(format!("line {} chain_id changed", line_index + 1));
            }
            None => {
                report.chain_id = Some(tip.chain_id.clone());
                chain_id = Some(tip.chain_id.clone());
            }
            _ => {}
        }
        if report.first_hash.is_none() {
            report.first_hash = Some(tip.this_hash.clone());
        }
        expected_previous = tip.this_hash.clone();
        expected_sequence = expected_sequence.saturating_add(1);
        report.last_hash = Some(tip.this_hash);
        report.last_prev_hash = Some(event_previous.to_string());
        report.last_sequence = Some(tip.sequence);
    }
    report
}

fn verify_chain_head(path: &Path, report: &mut ChainVerification) {
    let head_path = chain_head_path(path);
    if report.events == 0 {
        if head_path.exists() {
            report.valid = false;
            report.errors.push(format!(
                "audit log is empty but chain head exists: {}",
                head_path.display()
            ));
        }
        return;
    }
    let metadata = match fs::symlink_metadata(&head_path) {
        Ok(metadata) => metadata,
        Err(error) => {
            report.valid = false;
            report.errors.push(format!(
                "read audit chain head {}: {error}",
                head_path.display()
            ));
            return;
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        report.valid = false;
        report.errors.push(format!(
            "audit chain head is not a regular file: {}",
            head_path.display()
        ));
        return;
    }
    let data = match fs::read_to_string(&head_path) {
        Ok(data) => data,
        Err(error) => {
            report.valid = false;
            report.errors.push(format!(
                "read audit chain head {}: {error}",
                head_path.display()
            ));
            return;
        }
    };
    let head: serde_json::Value = match serde_json::from_str(&data) {
        Ok(head) => head,
        Err(error) => {
            report.valid = false;
            report.errors.push(format!(
                "parse audit chain head {}: {error}",
                head_path.display()
            ));
            return;
        }
    };
    let version = head
        .get("chain_version")
        .and_then(serde_json::Value::as_u64);
    let chain_id = head.get("chain_id").and_then(serde_json::Value::as_str);
    let sequence = head.get("sequence").and_then(serde_json::Value::as_u64);
    let this_hash = head.get("this_hash").and_then(serde_json::Value::as_str);
    let exact = version == Some(CHAIN_VERSION)
        && chain_id == report.chain_id.as_deref()
        && sequence == report.last_sequence
        && this_hash == report.last_hash.as_deref();
    let one_event_behind = version == Some(CHAIN_VERSION)
        && chain_id == report.chain_id.as_deref()
        && sequence
            .zip(report.last_sequence)
            .is_some_and(|(head, terminal)| head.checked_add(1) == Some(terminal))
        && this_hash == report.last_prev_hash.as_deref();
    if one_event_behind {
        report.warnings.push(
            "audit chain head is one event behind after a recoverable crash window".to_string(),
        );
    } else if !exact {
        report.valid = false;
        report
            .errors
            .push("audit chain head does not match the terminal event".to_string());
    }
}

fn first_event(bytes: &[u8]) -> Option<serde_json::Value> {
    std::str::from_utf8(bytes)
        .ok()?
        .lines()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| serde_json::from_str(line).ok())
}

struct ArchiveLink {
    path: PathBuf,
    sha256: String,
    kind: String,
}

fn archive_link(root: &Path, first: &serde_json::Value) -> Result<Option<ArchiveLink>, String> {
    let Some(relative) = first
        .get("previous_archive")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    let hash = first
        .get("previous_archive_sha256")
        .and_then(serde_json::Value::as_str)
        .filter(|value| valid_hash(value))
        .ok_or_else(|| "audit archive link has invalid sha256".to_string())?;
    let kind = first
        .get("previous_archive_kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("legacy");
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || relative_path
            .components()
            .next()
            .and_then(|component| match component {
                Component::Normal(value) => value.to_str(),
                _ => None,
            })
            != Some("archive")
    {
        return Err("audit archive link is not under the archive directory".to_string());
    }

    let linked = root.join(relative_path);
    let metadata = fs::symlink_metadata(&linked)
        .map_err(|error| format!("inspect linked archive {}: {error}", linked.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "linked archive is not a regular file: {}",
            linked.display()
        ));
    }
    Ok(Some(ArchiveLink {
        path: linked,
        sha256: hash.to_string(),
        kind: kind.to_string(),
    }))
}

fn estimate_chain_bytes(
    path: &Path,
    root: &Path,
    depth: usize,
    visited: &mut BTreeSet<PathBuf>,
) -> Result<u64, String> {
    if depth > MAX_ARCHIVE_DEPTH {
        return Err("audit archive chain exceeds maximum depth".to_string());
    }
    if !visited.insert(path.to_path_buf()) {
        return Err(format!(
            "audit archive cycle detected at {}",
            path.display()
        ));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "audit path is not a regular file: {}",
            path.display()
        ));
    }
    let mut total = metadata.len();
    let Some(first) = first_event_from_path(path)? else {
        return Ok(total);
    };
    let Some(link) = archive_link(root, &first)? else {
        return Ok(total);
    };
    let linked_size = fs::metadata(&link.path)
        .map_err(|error| format!("inspect {}: {error}", link.path.display()))?
        .len();
    if link.kind == "chain" {
        total = total
            .checked_add(estimate_chain_bytes(&link.path, root, depth + 1, visited)?)
            .ok_or_else(|| "audit archive byte count overflow".to_string())?;
    } else {
        total = total
            .checked_add(linked_size)
            .ok_or_else(|| "audit archive byte count overflow".to_string())?;
    }
    Ok(total)
}

fn first_event_from_path(path: &Path) -> Result<Option<serde_json::Value>, String> {
    use std::io::BufRead;

    let file = fs::File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if read == 0 {
            return Ok(None);
        }
        if line.len() > MAX_AUDIT_LINE_BYTES {
            return Err(format!(
                "audit event exceeds {} bytes in {}",
                MAX_AUDIT_LINE_BYTES,
                path.display()
            ));
        }
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str(line.trim())
            .map_err(|error| format!("parse first event in {}: {error}", path.display()))?;
        return Ok(Some(value));
    }
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_token(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// Capability-decision audit
// ---------------------------------------------------------------------------

/// Append a capability-decision record to `${log_dir}/caps.jsonl`.
///
/// Called by [`crate::caps::require`] on every check — both allows
/// and denials. The shape is intentionally stable so log consumers
/// (Agent permission history, permission-centre UI, downstream SIEMs)
/// can rely on the field names:
///
/// ```text
/// {
///   "ts":              "2026-05-13T20:58:00Z",  // UTC, ISO-8601
///   "session_id":      "s-1234",                // COS_SESSION
///   "pid":             4711,                    // caller pid
///   "agent":           "summarize",             // COS_AGENT_LABEL
///                                               //   or COS_APP_ID
///   "verb":            "ai.chat.untrusted",
///   "scope": {                                  // structured scope
///     "kind":  "name",
///     "value": "<configured-model>"
///   },
///   "target_resource": "<configured-model>",    // flattened scope
///   "decision":        "allow",                 // allow | deny
///   "reason":          null,                    // DenialReason kind
///   "hint":            null,                    // optional hint
///   "mode":            "strict"                 // strict | permissive
/// }
/// ```
///
/// Behaviour:
///   - Best-effort: IO failures are swallowed; enforcement never
///     blocks on the writer.
///   - Skips writing when `COS_CAPS_AUDIT=0` (used by the busy unit
///     tests so they don't spam the user's logs dir).
///   - `timestamp`, `trace_id`, and `span_id` come from
///     [`log_event`].
pub fn log_cap_decision(entry: serde_json::Value) {
    if std::env::var("COS_CAPS_AUDIT").as_deref() == Ok("0") {
        return;
    }
    crate::clawd::system_journal::record_cap_decision(&entry);
    let path = crate::paths::caps_audit_log_path();
    log_event(&path, entry);
}

#[cfg(test)]
mod cap_audit_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn log_cap_decision_writes_under_log_dir() {
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("COS_LOG_DIR");
        std::env::set_var("COS_LOG_DIR", dir.path());
        std::env::remove_var("COS_CAPS_AUDIT");
        log_cap_decision(json!({
            "session_id": "s-1",
            "verb": "fs.read",
            "decision": "allow",
        }));
        let p = crate::paths::caps_audit_log_path();
        assert!(p.is_file(), "expected {} to be a file", p.display());
        let body = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(v["session_id"], json!("s-1"));
        assert_eq!(v["verb"], json!("fs.read"));
        assert!(v["timestamp"].is_string());
        match prev {
            Some(v) => std::env::set_var("COS_LOG_DIR", v),
            None => std::env::remove_var("COS_LOG_DIR"),
        }
    }

    #[test]
    fn log_cap_decision_skipped_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("COS_LOG_DIR");
        std::env::set_var("COS_LOG_DIR", dir.path());
        std::env::set_var("COS_CAPS_AUDIT", "0");
        log_cap_decision(json!({
            "session_id": "s-1",
            "verb": "fs.read",
            "decision": "deny",
        }));
        let p = crate::paths::caps_audit_log_path();
        assert!(!p.exists(), "expected no caps.jsonl to be written");
        std::env::remove_var("COS_CAPS_AUDIT");
        match prev {
            Some(v) => std::env::set_var("COS_LOG_DIR", v),
            None => std::env::remove_var("COS_LOG_DIR"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_bearer_token() {
        let args = vec!["Bearer eyJhbGciOi...".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_bearer_case_insensitive() {
        let args = vec!["BEARER my-secret-token".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_token_prefix() {
        let args = vec!["token abc123".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_openai_key() {
        let args = vec!["sk-abc123def456".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_github_pat() {
        let args = vec!["ghp_xxxxxxxxxxxxxxxxxxxx".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_github_server_token() {
        let args = vec!["ghs_xxxxxxxxxxxxxxxxxxxx".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_gitlab_token() {
        let args = vec!["glpat-xxxxxxxxxxxxxxxxxxxx".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_slack_bot_token() {
        let args = vec!["xoxb-123-456-abc".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_slack_user_token() {
        let args = vec!["xoxp-123-456-abc".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_authorization_header() {
        let args = vec!["Authorization: Bearer secret".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["Authorization: ***REDACTED***"]);
    }

    #[test]
    fn redact_authorization_header_case_insensitive() {
        let args = vec!["authorization:basic dXNlcjpwYXNz".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["Authorization: ***REDACTED***"]);
    }

    #[test]
    fn safe_args_pass_through() {
        let args = vec![
            "--output".to_string(),
            "json".to_string(),
            "/path/to/file".to_string(),
        ];
        let result = redact_args(&args);
        assert_eq!(result, args);
    }

    #[test]
    fn mixed_safe_and_sensitive_args() {
        let args = vec![
            "--header".to_string(),
            "Authorization: Bearer secret".to_string(),
            "--url".to_string(),
            "https://api.example.com".to_string(),
        ];
        let result = redact_args(&args);
        assert_eq!(result[0], "--header");
        assert_eq!(result[1], "Authorization: ***REDACTED***");
        assert_eq!(result[2], "--url");
        assert_eq!(result[3], "https://api.example.com");
    }

    #[test]
    fn empty_args() {
        let args: Vec<String> = vec![];
        let result = redact_args(&args);
        assert!(result.is_empty());
    }

    // ---- log_event ----

    #[test]
    fn log_event_appends_jsonl_with_auto_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        log_event(&p, json!({ "kind": "smoke", "n": 1 }));
        log_event(&p, json!({ "kind": "smoke", "n": 2 }));
        let body = std::fs::read_to_string(&p).unwrap();
        let mut lines = body.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines.remove(0)).unwrap();
        assert_eq!(v["kind"], json!("smoke"));
        assert_eq!(v["n"], json!(1));
        assert!(v["timestamp"].is_string(), "auto-timestamp should be added");
    }

    #[test]
    fn log_event_preserves_caller_supplied_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        log_event(
            &p,
            json!({ "kind": "x", "timestamp": "2099-01-01T00:00:00Z" }),
        );
        let body = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(v["timestamp"], json!("2099-01-01T00:00:00Z"));
    }

    #[test]
    fn log_event_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("a").join("audit.jsonl");
        log_event(&p, json!({ "kind": "x" }));
        assert!(p.exists());
    }

    #[test]
    fn log_event_swallows_non_object_entries_via_no_inject() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        // Top-level array is legal JSON; we just don't inject
        // timestamp/trace_id into it. Should still be appended.
        log_event(&p, json!(["raw", "values"]));
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body.trim(), "[\"raw\",\"values\"]");
    }
}
