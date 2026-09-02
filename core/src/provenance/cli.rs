//! `cos provenance …` — the operator and developer surface.
//!
//! Commands:
//!
//! | Command | Purpose |
//! | --- | --- |
//! | `keygen` | Create a publisher signing key (mode `0600`) plus its trust entry |
//! | `sign` | Build and sign a package's `claw.provenance/v1` envelope |
//! | `verify` | Authenticate a package directory against the active trust store |
//! | `trust list` | Show trusted keys, revocations and developer grants |
//! | `trust add` | Install a publisher trust entry into the per-user root |
//! | `trust revoke` | Revoke a key id or a package content digest |
//! | `dev-trust` | Record a persistent decision to run one unsigned tree |
//! | `dev-untrust` | Withdraw a developer grant |
//! | `artifacts` | List retained verified artifacts for a package |
//! | `rollback` | Re-activate a previously verified artifact |
//!
//! Every mutation reloads the trust store and invalidates cached
//! verifications, so a revocation takes effect for the next launch,
//! disclosure or attach without a restart.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::envelope::PackageKind;
use super::install;
use super::sign::{self, SigningKeyFile};
use super::trust::{self, DevGrant, TrustStore};
use super::verify::{self, VerifyOptions};

/// Commands that change or consume trust need the ownership, `openat`
/// and durability guarantees the verifier is built on. Rather than
/// return a hollow success on a host that cannot provide them, they are
/// refused outright.
fn require_unix(command: &str) -> Result<(), String> {
    if cfg!(unix) {
        return Ok(());
    }
    Err(crate::errors::error(
        "provenance.unsupported",
        &format!(
            "`cos provenance {command}` requires a Unix host: package verification depends on \
             POSIX ownership/mode checks, `openat`-based reads and durable renames that this \
             platform cannot provide. No partial or advisory result is offered."
        ),
    )
    .to_string())
}

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "keygen" | "sign" | "verify" | "trust" | "dev-trust" | "dev-untrust" | "artifacts"
        | "rollback" => require_unix(command)?,
        _ => {}
    }
    match command {
        "keygen" => keygen(args),
        "sign" => sign_cmd(args),
        "verify" => verify_cmd(args),
        "trust" => trust_cmd(args),
        "dev-trust" => dev_trust(args),
        "dev-untrust" => dev_untrust(args),
        "artifacts" => artifacts(args),
        "rollback" => rollback(args),
        other => Err(format!(
            "unknown provenance command `{other}`; try: keygen, sign, verify, trust, dev-trust, dev-untrust, artifacts, rollback"
        )),
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let long = format!("--{name}");
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == &long {
            return iter.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix(&format!("{long}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    let long = format!("--{name}");
    let mut out = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == &long {
            if let Some(v) = iter.next() {
                out.push(v.clone());
            }
        } else if let Some(rest) = arg.strip_prefix(&format!("{long}=")) {
            out.push(rest.to_string());
        }
    }
    out
}

fn require(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or_else(|| format!("missing required flag --{name}"))
}

fn parse_kind(args: &[String]) -> Result<PackageKind, String> {
    let raw = require(args, "kind")?;
    PackageKind::parse(&raw)
        .ok_or_else(|| format!("unknown package kind `{raw}` (app|skill|mcp|extension)"))
}

fn keygen(args: &[String]) -> Result<Value, String> {
    let out = PathBuf::from(require(args, "out")?);
    let comment = flag(args, "comment");
    let key = SigningKeyFile::generate(comment).map_err(|e| e.to_string())?;
    key.write_new(&out).map_err(|e| e.to_string())?;
    let kinds = [
        PackageKind::App,
        PackageKind::Skill,
        PackageKind::Mcp,
        PackageKind::AgentExtension,
    ];
    super::audit(
        "provenance.keygen",
        json!({ "key_id": key.key_id, "path": out.display().to_string() }),
    );
    Ok(json!({
        "key_id": key.key_id,
        "public_key": key.public_key,
        "private_key_path": out.display().to_string(),
        "trust_entry": key.trust_entry(&kinds),
        "hint": "Never commit the private key. Publish only the `trust_entry` object.",
    }))
}

fn sign_cmd(args: &[String]) -> Result<Value, String> {
    let kind = parse_kind(args)?;
    let dir = PathBuf::from(require(args, "path")?);
    let key_path = PathBuf::from(require(args, "key")?);
    let id = require(args, "id")?;
    let version = flag(args, "version").unwrap_or_else(|| "0.0.0".to_string());
    let manifest_path = flag(args, "manifest").unwrap_or_else(|| kind.manifest_file().to_string());
    let manifest_schema = flag(args, "manifest-schema").unwrap_or_else(|| match kind {
        PackageKind::App => "cos.app-manifest/v1".to_string(),
        PackageKind::Skill => "agentskills.io/skill-md/v1".to_string(),
        PackageKind::Mcp => "claw.agent-api/v1".to_string(),
        PackageKind::AgentExtension => "claw.agent-extension/v1".to_string(),
    });
    let key = SigningKeyFile::load(&key_path).map_err(|e| e.to_string())?;
    let request = sign::SignRequest {
        kind,
        id: id.clone(),
        version,
        manifest_schema,
        manifest_path,
        entrypoints: flags(args, "entrypoint"),
        resources: flags(args, "resource"),
    };
    let envelope = sign::sign_directory(&dir, &request, &key).map_err(|e| e.to_string())?;
    super::audit(
        "provenance.signed",
        json!({
            "package_kind": kind.as_str(),
            "package_id": id,
            "content_digest": envelope.package.content_digest,
            "publisher_key_id": envelope.signature.key_id,
        }),
    );
    Ok(json!({
        "signed": true,
        "kind": kind.as_str(),
        "id": envelope.package.id,
        "version": envelope.package.version,
        "content_digest": envelope.package.content_digest,
        "key_id": envelope.signature.key_id,
        "files": envelope.package.files.len(),
        "envelope": dir.join(super::envelope::ENVELOPE_FILE).display().to_string(),
    }))
}

fn verify_cmd(args: &[String]) -> Result<Value, String> {
    let kind = parse_kind(args)?;
    let dir = PathBuf::from(require(args, "path")?);
    let trust = super::trust_store();
    let mut options = VerifyOptions::new(kind);
    if let Some(id) = flag(args, "id") {
        options = options.expect_id(id);
    }
    match verify::verify_package(&dir, &options, &trust) {
        Ok(pkg) => Ok(json!({
            "verified": true,
            "package": pkg.audit_facts(),
            "manifest_path": pkg.manifest_path(),
            "entrypoints": pkg.entrypoints(),
        })),
        Err(e) => Err(crate::errors::error(e.code(), &e.to_string()).to_string()),
    }
}

fn trust_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    let rest = if args.is_empty() { args } else { &args[1..] };
    match sub {
        "list" => trust_list(),
        "add" => trust_add(rest),
        "revoke" => trust_revoke(rest),
        "roots" => Ok(json!({
            "roots": TrustStore::default_roots()
                .iter()
                .map(|r| json!({
                    "path": r.path.display().to_string(),
                    "tier": r.tier.as_str(),
                    "allowed_uids": r.allowed_uids,
                }))
                .collect::<Vec<_>>(),
            "note": "Trust roots are compiled in. No environment variable can add one.",
        })),
        other => Err(format!(
            "unknown trust command `{other}`; try: list, roots, add, revoke"
        )),
    }
}

fn trust_list() -> Result<Value, String> {
    let store = super::reload_trust();
    let keys: Vec<Value> = store
        .keys()
        .map(|k| {
            json!({
                "key_id": k.key_id,
                "tier": k.tier.as_str(),
                "kinds": k.kinds.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                "usages": k.usages,
                "not_before": k.validity.not_before.map(|t| t.to_rfc3339()),
                "not_after": k.validity.not_after.map(|t| t.to_rfc3339()),
                "source": k.source.display().to_string(),
                "comment": k.comment,
            })
        })
        .collect();
    let grants: Vec<Value> = store
        .dev_grants()
        .map(|g| {
            json!({
                "kind": g.kind.as_str(),
                "id": g.id,
                "path": g.path.display().to_string(),
                "content_digest": g.content_digest,
                "granted_at": g.granted_at,
                "note": g.note,
            })
        })
        .collect();
    Ok(json!({
        "generation": store.generation(),
        "keys": keys,
        "developer_grants": grants,
        "diagnostics": store.diagnostics(),
    }))
}

fn user_trust_file() -> Result<PathBuf, String> {
    Ok(trust::user_trust_dir()?.join("operator.json"))
}

fn read_trust_file(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| {
            json!({
                "schema": trust::TRUST_SCHEMA_V1,
                "keys": [],
                "revoked_keys": [],
                "revoked_packages": [],
            })
        })
}

/// Re-record the owner domain's durable generation after a mutation.
///
/// Without this a daemon would keep serving the previous store: the
/// generation is what its cheap staleness check compares against.
///
/// The domain is always the *caller's own*: the uid comes from
/// `geteuid`, the directory from that uid's verified passwd home, and
/// `write_owner_file` refuses a path the caller does not own. There is
/// no argument for "which owner", so one user cannot revoke — or
/// restore — another user's trust. System-wide roots under `/etc/cos`
/// and `/usr/lib/cos` are root-owned and are managed by the package
/// manager, not by this command.
fn record_owner_generation() -> Result<u64, String> {
    #[cfg(unix)]
    {
        let uid = crate::provenance::fsec::effective_uid();
        let home = crate::paths::verified_home_for_uid(uid)?;
        let dir = home.join(".config/cos/trust");
        let roots = vec![dir.join("publishers.d"), dir.join("developer.d")];
        let state = crate::provenance::state::bump(
            &dir,
            crate::provenance::state::TrustDomain::Owner(uid),
            &roots,
        )
        .map_err(|e| format!("record trust generation: {e}"))?;
        Ok(state.generation)
    }
    #[cfg(not(unix))]
    {
        Err("recording trust generation requires a Unix host".to_string())
    }
}

fn write_owner_file(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let body = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("publish {}: {e}", path.display()))?;
    Ok(())
}

fn trust_add(args: &[String]) -> Result<Value, String> {
    let source = PathBuf::from(require(args, "file")?);
    let raw =
        std::fs::read_to_string(&source).map_err(|e| format!("read {}: {e}", source.display()))?;
    let incoming: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if incoming.get("schema").and_then(Value::as_str) != Some(trust::TRUST_SCHEMA_V1) {
        return Err(format!(
            "trust entry must declare schema `{}`",
            trust::TRUST_SCHEMA_V1
        ));
    }
    if incoming.get("private_key").is_some() {
        return Err("refusing to install a file containing private key material".to_string());
    }
    let path = user_trust_file()?;
    let mut current = read_trust_file(&path);
    let mut added = Vec::new();
    if let Some(keys) = incoming.get("keys").and_then(Value::as_array) {
        let list = current
            .get_mut("keys")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "existing trust file is malformed".to_string())?;
        for key in keys {
            let id = key
                .get("key_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if list
                .iter()
                .any(|k| k.get("key_id").and_then(Value::as_str) == Some(id))
            {
                continue;
            }
            list.push(key.clone());
            added.push(id.to_string());
        }
    }
    write_owner_file(&path, &current)?;
    let domain_generation = record_owner_generation()?;
    let store = super::reload_trust();
    for id in &added {
        super::audit("provenance.trust_added", json!({ "publisher_key_id": id }));
    }
    Ok(json!({
        "trust_file": path.display().to_string(),
        "added": added,
        "generation": store.generation(),
        "domain_generation": domain_generation,
        "diagnostics": store.diagnostics(),
    }))
}

fn trust_revoke(args: &[String]) -> Result<Value, String> {
    let key_id = flag(args, "key-id");
    let digest = flag(args, "digest");
    if key_id.is_none() && digest.is_none() {
        return Err("pass --key-id <sha256:…> and/or --digest <sha256:…>".to_string());
    }
    let path = user_trust_file()?;
    let mut current = read_trust_file(&path);
    if let Some(id) = &key_id {
        let list = current
            .get_mut("revoked_keys")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "existing trust file is malformed".to_string())?;
        if !list.iter().any(|v| v.as_str() == Some(id.as_str())) {
            list.push(Value::String(id.clone()));
        }
    }
    if let Some(d) = &digest {
        let list = current
            .get_mut("revoked_packages")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "existing trust file is malformed".to_string())?;
        if !list.iter().any(|v| v.as_str() == Some(d.as_str())) {
            list.push(Value::String(d.clone()));
        }
    }
    write_owner_file(&path, &current)?;
    let domain_generation = record_owner_generation()?;
    let store = super::reload_trust();
    // No notification is sent, and none is needed. The generation this
    // just bumped is the hot path: every daemon re-stats it before each
    // authority decision, relay and tool call, and rebuilds the store
    // when it moved. A message could only ever be an optimisation on
    // top of that, and a message that failed to arrive must never be
    // the difference between revoked and not.
    //
    // What this pass does add is the *local* half: instances this
    // process can see are stopped now rather than at the next
    // supervision tick.
    let report = super::runtime::lifecycle_tick(
        super::runtime::current_owner(),
        &store,
        super::runtime::SHUTDOWN_GRACE,
    );
    super::audit(
        "provenance.revoked",
        json!({
            "publisher_key_id": key_id,
            "content_digest": digest,
            "marked_sessions": report.marked.len(),
            "terminated_sessions": report.terminated.len(),
        }),
    );
    Ok(json!({
        "revoked_key": key_id,
        "revoked_digest": digest,
        "generation": store.generation(),
        "domain_generation": domain_generation,
        "sessions_marked_for_shutdown": report.marked,
        "sessions_terminated": report.terminated,
        "note": "Cached verifications were invalidated. A running instance is denied on its very next authority call, relay or tool call; an idle one is stopped by the next supervision pass. Neither waits for a grant to expire and neither needs a daemon restart.",
    }))
}

fn dev_trust_file() -> Result<PathBuf, String> {
    Ok(trust::developer_trust_dir()?.join("grants.json"))
}

fn read_dev_file(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({ "schema": trust::DEV_TRUST_SCHEMA_V1, "grants": [] }))
}

fn dev_trust(args: &[String]) -> Result<Value, String> {
    let kind = parse_kind(args)?;
    let id = require(args, "id")?;
    let dir = PathBuf::from(require(args, "path")?)
        .canonicalize()
        .map_err(|e| format!("resolve --path: {e}"))?;
    let note = flag(args, "note");

    // Compute the digest by verifying with developer trust disabled;
    // the resulting error carries the reason the package is unsigned,
    // while the scan itself proves the tree is free of symlinks,
    // hardlinks and special files.
    let request = sign::SignRequest {
        kind,
        id: id.clone(),
        version: "dev".to_string(),
        manifest_schema: "developer".to_string(),
        manifest_path: kind.manifest_file().to_string(),
        entrypoints: Vec::new(),
        resources: Vec::new(),
    };
    let body = sign::build_body(&dir, &request).map_err(|e| e.to_string())?;
    let digest = super::envelope::content_digest(&body.files);

    // Trusting unsigned code is the one decision that must come from a
    // person. No flag, environment variable or model-issued tool call
    // satisfies this; automation uses an offline signed grant file.
    let developer_root = trust::developer_trust_dir()?;
    super::consent::require_developer_consent(
        kind,
        &id,
        &dir,
        &digest,
        &developer_root,
        args.iter().any(|a| a == "--yes"),
    )
    .map_err(|e| crate::errors::error("provenance.consent_required", &e.to_string()).to_string())?;

    let grant = DevGrant {
        kind,
        id: id.clone(),
        path: dir.clone(),
        content_digest: digest.clone(),
        granted_at: chrono::Utc::now().to_rfc3339(),
        note,
    };
    let path = dev_trust_file()?;
    let mut current = read_dev_file(&path);
    let list = current
        .get_mut("grants")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "existing developer trust file is malformed".to_string())?;
    list.retain(|g| {
        !(g.get("kind").and_then(Value::as_str) == Some(kind.as_str())
            && g.get("id").and_then(Value::as_str) == Some(id.as_str()))
    });
    list.push(serde_json::to_value(&grant).map_err(|e| e.to_string())?);
    write_owner_file(&path, &current)?;
    let domain_generation = record_owner_generation()?;
    let store = super::reload_trust();
    super::audit(
        "provenance.developer_trusted",
        json!({
            "package_kind": kind.as_str(),
            "package_id": id,
            "content_digest": digest,
            "path": dir.display().to_string(),
        }),
    );
    Ok(json!({
        "developer_trust": true,
        "kind": kind.as_str(),
        "id": id,
        "path": dir.display().to_string(),
        "content_digest": digest,
        "ceiling": super::Ceiling::for_tier(super::TrustTier::Developer).facts(),
        "generation": store.generation(),
        "domain_generation": domain_generation,
        "warning": "Unsigned developer content runs with a restricted capability ceiling and no privileged routes. Editing the tree invalidates this grant.",
    }))
}

fn dev_untrust(args: &[String]) -> Result<Value, String> {
    let kind = parse_kind(args)?;
    let id = require(args, "id")?;
    let path = dev_trust_file()?;
    let mut current = read_dev_file(&path);
    let list = current
        .get_mut("grants")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "existing developer trust file is malformed".to_string())?;
    let before = list.len();
    list.retain(|g| {
        !(g.get("kind").and_then(Value::as_str) == Some(kind.as_str())
            && g.get("id").and_then(Value::as_str) == Some(id.as_str()))
    });
    let removed = before - list.len();
    write_owner_file(&path, &current)?;
    let domain_generation = record_owner_generation()?;
    let store = super::reload_trust();
    let report = super::runtime::lifecycle_tick(
        super::runtime::current_owner(),
        &store,
        super::runtime::SHUTDOWN_GRACE,
    );
    super::audit(
        "provenance.developer_untrusted",
        json!({ "package_kind": kind.as_str(), "package_id": id }),
    );
    Ok(json!({
        "removed": removed,
        "kind": kind.as_str(),
        "id": id,
        "generation": store.generation(),
        "domain_generation": domain_generation,
        "sessions_marked_for_shutdown": report.marked.len(),
        "sessions_terminated": report.terminated.len(),
    }))
}

fn artifacts(args: &[String]) -> Result<Value, String> {
    let kind = parse_kind(args)?;
    let id = require(args, "id")?;
    let dirs = install::list_artifacts(kind, &id);
    let trust = super::trust_store();
    let options = VerifyOptions::new(kind).expect_id(&id).signature_only();
    let entries: Vec<Value> = dirs
        .iter()
        .map(|dir| match verify::verify_package(dir, &options, &trust) {
            Ok(pkg) => json!({
                "path": dir.display().to_string(),
                "content_digest": pkg.content_digest(),
                "version": pkg.version(),
                "verifiable": true,
            }),
            Err(e) => json!({
                "path": dir.display().to_string(),
                "verifiable": false,
                "error": e.to_string(),
            }),
        })
        .collect();
    Ok(json!({ "kind": kind.as_str(), "id": id, "artifacts": entries }))
}

/// Resolve a user-supplied digest to the one canonical full digest of a
/// retained artifact.
///
/// Artifact directories are named with a 32-character prefix, so a
/// caller who copies the short name out of `cos provenance artifacts`
/// would otherwise get "no retained artifact" for a digest that plainly
/// exists. A unique prefix resolves; an ambiguous one lists the
/// candidates instead of guessing.
fn resolve_artifact_digest(kind: PackageKind, id: &str, supplied: &str) -> Result<String, String> {
    let normalised = supplied.trim().to_ascii_lowercase();
    let bare = normalised
        .strip_prefix("sha256:")
        .unwrap_or(&normalised)
        .to_string();
    if bare.is_empty() || !bare.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "`{supplied}` is not a hex digest; pass the full `sha256:<64 hex>` value \
             from `cos provenance artifacts --kind {} --id {id}`",
            kind.as_str()
        ));
    }
    let trust = super::trust_store();
    let options = VerifyOptions::new(kind).expect_id(id).signature_only();
    let mut matches: Vec<String> = Vec::new();
    for dir in install::list_artifacts(kind, id) {
        if let Ok(pkg) = verify::verify_package(&dir, &options, &trust) {
            let full = pkg.content_digest().to_string();
            let full_bare = full.strip_prefix("sha256:").unwrap_or(&full).to_string();
            if (full_bare == bare || full_bare.starts_with(&bare)) && !matches.contains(&full) {
                matches.push(full);
            }
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(format!(
            "no retained, still-verifiable artifact for {} `{id}` matches digest `{supplied}`; \
             run `cos provenance artifacts --kind {} --id {id}` to list them",
            kind.as_str(),
            kind.as_str()
        )),
        _ => Err(format!(
            "digest `{supplied}` is ambiguous across {} retained artifacts ({}); \
             pass the full sha256 value",
            matches.len(),
            matches.join(", ")
        )),
    }
}

fn rollback(args: &[String]) -> Result<Value, String> {
    let kind = parse_kind(args)?;
    let id = require(args, "id")?;
    let digest = resolve_artifact_digest(kind, &id, &require(args, "digest")?)?;
    let dest = PathBuf::from(require(args, "dest")?);
    let trust = super::trust_store();
    let published = install::rollback(
        kind,
        &id,
        &digest,
        &dest,
        &trust,
        &install::Limits::default(),
    )
    .map_err(|e| crate::errors::error(e.code(), &e.to_string()).to_string())?;
    super::audit(
        "provenance.rollback",
        json!({
            "package_kind": kind.as_str(),
            "package_id": id,
            "content_digest": published.content_digest,
        }),
    );
    Ok(json!({
        "rolled_back": true,
        "kind": kind.as_str(),
        "id": id,
        "content_digest": published.content_digest,
        "live_dir": published.live_dir.display().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/provenance/cli.rs"
    ));
}
