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
//! Public App rollback uses the router's Root orchestration, not local publication here.

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
        "status" | "recover" => Err("Trust withdrawal status/recovery requires the public router and authenticated Root controller".into()),
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
            "unknown trust command `{other}`; try: list, roots, add, revoke, status, recover"
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
    let target = super::withdrawal::Target::Revocation {
        key_id: key_id.clone(), digest: digest.clone(),
    };
    target.validate()?;
    let path = user_trust_file()?;
    let _lock = WithdrawalLock::acquire(&path)?;
    let mut current = read_withdrawal_file(&path, false)?;
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
    write_withdrawal_file(&path, &current)?;
    let generation = record_owner_generation();
    let store = super::reload_trust();
    super::audit(
        "provenance.revoked",
        json!({
            "publisher_key_id": key_id,
            "content_digest": digest,
            "policy_withdrawn": true,
            "root_retirement_confirmed": false,
        }),
    );
    Ok(json!({
        "revoked_key": key_id,
        "revoked_digest": digest,
        "generation": store.generation(),
        "domain_generation": generation.as_ref().ok(),
        "generation_error": generation.err(),
        "policy_withdrawn": true,
        "policy_scope": "effective_uid_private_file",
        "root_retirement": { "scope": "apps_only", "root_retirement_confirmed": false, "state": "unconfirmed" },
        "non_app_retirement": { "confirmed": false, "state": "unknown" },
        "withdrawal_target": target,
        "note": "Own policy was narrowed without waiting for cleanup. Only the authenticated Root controller can confirm App retirement; local process inspection is not a retirement receipt.",
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
    let target = super::withdrawal::Target::Developer { kind, id: id.clone() };
    target.validate()?;
    let path = dev_trust_file()?;
    let _lock = WithdrawalLock::acquire(&path)?;
    let mut current = read_withdrawal_file(&path, true)?;
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
    write_withdrawal_file(&path, &current)?;
    let generation = record_owner_generation();
    let store = super::reload_trust();
    super::audit(
        "provenance.developer_untrusted",
        json!({ "package_kind": kind.as_str(), "package_id": id }),
    );
    Ok(json!({
        "removed": removed,
        "kind": kind.as_str(),
        "id": id,
        "generation": store.generation(),
        "domain_generation": generation.as_ref().ok(),
        "generation_error": generation.err(),
        "policy_withdrawn": true,
        "policy_scope": "effective_uid_private_file",
        "developer_grant_remaining": store.diagnostics().is_empty().then(|| store.dev_grant(kind, &id).is_some()),
        "root_retirement": { "scope": "apps_only", "root_retirement_confirmed": false, "state": "unconfirmed" },
        "non_app_retirement": { "confirmed": false, "state": "unknown" },
        "withdrawal_target": target,
    }))
}

pub enum WithdrawalCommand {
    Reconcile(super::withdrawal::Target),
    Status(String),
    Recover(String),
}

pub fn withdrawal_command(command: &str, args: &[String]) -> Result<Option<WithdrawalCommand>, String> {
    let selected = match command {
        "trust" if args.first().is_some_and(|arg| arg == "revoke") => {
            WithdrawalCommand::Reconcile(super::withdrawal::Target::Revocation {
                key_id: flag(&args[1..], "key-id"),
                digest: flag(&args[1..], "digest"),
            })
        }
        "dev-untrust" => WithdrawalCommand::Reconcile(super::withdrawal::Target::Developer {
            kind: parse_kind(args)?, id: require(args, "id")?,
        }),
        "trust" if args.first().is_some_and(|arg| arg == "status" || arg == "recover") => {
            if args.len() != 2 || args[1].len() != 32
                || !args[1].bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err("usage: cos provenance trust status|recover <32-lowercase-hex-request-id>".into());
            }
            if args[0] == "status" { WithdrawalCommand::Status(args[1].clone()) }
            else { WithdrawalCommand::Recover(args[1].clone()) }
        }
        _ => return Ok(None),
    };
    require_unix(command)?;
    if let WithdrawalCommand::Reconcile(target) = &selected { target.validate()?; }
    Ok(Some(selected))
}

fn read_withdrawal_file(path: &Path, developer: bool) -> Result<Value, String> {
    let parent = path.parent().ok_or("own trust file has no parent")?;
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(if developer {
                json!({ "schema": trust::DEV_TRUST_SCHEMA_V1, "grants": [] })
            } else {
                json!({ "schema": trust::TRUST_SCHEMA_V1, "keys": [], "revoked_keys": [], "revoked_packages": [] })
            });
        }
        Err(error) => return Err(format!("inspect own trust policy: {error}")),
        Ok(_) => {}
    }
    let metadata = super::fsec::require_secure_location(path, &[super::fsec::effective_uid()])
        .map_err(|error| error.to_string())?;
    if !metadata.is_file {
        return Err("own trust policy is not a regular file".into());
    }
    #[cfg(unix)]
    {
        let directory = super::fsec::DirHandle::open(parent).map_err(|error| error.to_string())?;
        let file = directory.open_file(path.file_name().and_then(|name| name.to_str()).ok_or("trust file has no UTF-8 name")?)
            .map_err(|error| error.to_string())?;
        let bytes = file.read_bounded(1024 * 1024).map_err(|error| error.to_string())?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| format!("own trust policy is malformed: {error}"))?;
        let schema = if developer { trust::DEV_TRUST_SCHEMA_V1 } else { trust::TRUST_SCHEMA_V1 };
        if value.get("schema").and_then(Value::as_str) != Some(schema) {
            return Err("own trust policy has an unsupported schema; no revocations were discarded".into());
        }
        Ok(value)
    }
    #[cfg(not(unix))]
    { let _ = parent; Err("own policy withdrawal requires Unix".into()) }
}

fn write_withdrawal_file(path: &Path, value: &Value) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("own trust policy has no parent")?;
    create_withdrawal_directory(parent)?;
    let metadata = super::fsec::require_secure_location(parent, &[super::fsec::effective_uid()])
        .map_err(|error| error.to_string())?;
    if !metadata.is_dir { return Err("own trust policy parent is not protected".into()); }
    let temporary = parent.join(format!(".withdrawal-{}", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let body = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
        if body.len() > 1024 * 1024 {
            return Err("own withdrawal policy would exceed its bounded file size".into());
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temporary).map_err(|error| error.to_string())?;
        file.write_all(&body).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())?;
        super::fsec::sync_dir(parent).map_err(|error| error.to_string())
    })();
    let _ = std::fs::remove_file(temporary);
    result.map_err(|error| format!("Own policy withdrawal durability is unconfirmed: {error}; Root retirement is unconfirmed"))
}

fn create_withdrawal_directory(path: &Path) -> Result<(), String> {
    let mut missing = Vec::new();
    let mut ancestor = path;
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(ancestor.to_path_buf());
                if missing.len() > 64 { return Err("own trust directory depth exceeds its bound".into()); }
                ancestor = ancestor.parent().ok_or("own trust directory has no protected ancestor")?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    super::fsec::require_secure_location(ancestor, &[0, super::fsec::effective_uid()])
        .map_err(|error| error.to_string())?;
    for directory in missing.into_iter().rev() {
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
        super::fsec::require_secure_location(&directory, &[super::fsec::effective_uid()])
            .map_err(|error| error.to_string())?;
        super::fsec::sync_dir(&directory).map_err(|error| error.to_string())?;
        super::fsec::sync_dir(directory.parent().ok_or("own trust directory lost its parent")?)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

struct WithdrawalLock(std::fs::File);

impl WithdrawalLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        let directory = path.parent().and_then(Path::parent).ok_or("own trust domain has no directory")?;
        create_withdrawal_directory(directory)?;
        let metadata = super::fsec::require_secure_location(directory, &[super::fsec::effective_uid()])
            .map_err(|error| error.to_string())?;
        if !metadata.is_dir { return Err("own trust domain is not protected".into()); }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            let file = std::fs::OpenOptions::new().read(true).write(true).create(true)
                .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(directory.join(".withdrawal.lock")).map_err(|error| error.to_string())?;
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.uid() != super::fsec::effective_uid()
                || metadata.nlink() != 1 || metadata.mode() & 0o077 != 0
            {
                return Err("own withdrawal lock is not a protected single-link file".into());
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    return Ok(Self(file));
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::WouldBlock || std::time::Instant::now() >= deadline {
                    return Err(format!("own withdrawal serialization is unavailable: {error}; policy withdrawal is unconfirmed"));
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        #[cfg(not(unix))]
        { Err("own policy withdrawal requires Unix".into()) }
    }
}

impl Drop for WithdrawalLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN); }
        }
    }
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

/// Read-only App rollback selection for the public frontend's Root orchestration.
pub(crate) fn resolve_app_rollback(
    args: &[String],
) -> Result<Option<(verify::VerifiedPackage, PathBuf)>, String> {
    require_unix("rollback")?;
    let kind = parse_kind(args)?;
    if kind != PackageKind::App {
        return Ok(None);
    }
    let id = require(args, "id")?;
    let digest = resolve_artifact_digest(kind, &id, &require(args, "digest")?)?;
    let dest = PathBuf::from(require(args, "dest")?);
    let package = install::retained_artifact(
        kind,
        &id,
        &digest,
        &super::trust_store(),
        &install::Limits::default(),
    )
    .map_err(|error| crate::errors::error(error.code(), &error.to_string()).to_string())?;
    Ok(Some((package, dest)))
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
