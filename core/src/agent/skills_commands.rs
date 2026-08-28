use super::skills;
use serde_json::{json, Value};

/// `cos agent skills [list|info <id>|disabled|errors|root]` — exposes
/// the on-disk skill registry under `data_dir/agent/skills/`.
pub(super) fn skills_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("list");
    match sub {
        "root" => Ok(json!({
            "root": crate::paths::agent_skills_dir().display().to_string(),
            "user_root": crate::paths::agent_skills_dir().display().to_string(),
            "system_root": crate::paths::system_skills_dir().display().to_string(),
        })),
        "list" | "" => {
            let res = skills::loader::load_default();
            let names: Vec<&String> = res.skills.keys().collect();
            Ok(json!({
                "root": crate::paths::agent_skills_dir().display().to_string(),
                "user_root": crate::paths::agent_skills_dir().display().to_string(),
                "system_root": crate::paths::system_skills_dir().display().to_string(),
                "loaded": res.loaded_count(),
                "disabled": res.disabled.len(),
                "errors": res.errors.len(),
                "names": names,
            }))
        }
        "info" => {
            let id = args.get(1).cloned().unwrap_or_default();
            if id.is_empty() {
                return Err("usage: cos agent skills info <id>".into());
            }
            let res = skills::loader::load_default();
            if let Some(s) = res.skills.get(&id) {
                Ok(json!({
                    "id": s.id,
                    "dir": s.dir.display().to_string(),
                    "manifest_path": s.manifest_path.display().to_string(),
                    "name": s.manifest.name,
                    "description": s.manifest.description,
                    "source": s.origin.as_str(),
                    "version": s.manifest.version,
                    "license": s.manifest.license,
                    "author": s.manifest.author,
                    "homepage": s.manifest.homepage,
                    "allowed_tools": s.manifest.allowed_tools,
                    "triggers": s.manifest.triggers,
                    "body_bytes": s.body_bytes,
                    "disclosable": skills::disclosure::instruction_disclosable(s),
                }))
            } else if let Some(reason) = res.disabled.get(&id) {
                Ok(json!({
                    "id": id,
                    "status": "disabled",
                    "reason": reason,
                }))
            } else if let Some(err) = res.errors.get(&id) {
                Ok(json!({
                    "id": id,
                    "status": "error",
                    "error": err,
                }))
            } else {
                Err(format!("unknown skill: {id}"))
            }
        }
        "disabled" => {
            let res = skills::loader::load_default();
            Ok(json!({
                "n": res.disabled.len(),
                "disabled": res.disabled,
            }))
        }
        "errors" => {
            let res = skills::loader::load_default();
            Ok(json!({
                "n": res.errors.len(),
                "errors": res.errors,
            }))
        }
        "install" => {
            let archive = args.get(1).cloned().unwrap_or_default();
            if archive.is_empty() {
                return Err(
                    "usage: cos agent skills install <archive.zip> [--force]".into(),
                );
            }
            let force = args.iter().any(|a| a == "--force" || a == "-f");
            let path = std::path::PathBuf::from(&archive);
            match skills::sync::install_from_archive(&path, force) {
                Ok(res) => Ok(json!({
                    "ok": true,
                    "id": res.id,
                    "install_dir": res.install_dir.display().to_string(),
                    "files_extracted": res.files_extracted,
                    "bytes_on_disk": res.bytes_on_disk,
                    "replaced_existing": res.replaced_existing,
                })),
                Err(e) => Err(format!("install failed: {e}")),
            }
        }
        "hub" => skills_hub_cmd(&args[1..]),
        "usage" => skills_usage_cmd(&args[1..]),
        "guard" => skills_guard_cmd(&args[1..]),
        other => Err(format!(
            "unknown skills subcommand: {other}. try: list | info <id> | disabled | errors | root | install <archive> | hub <list|show|install> <owner/repo> [<id>] | usage <stats|record|path|clear> | guard <id> [--provenance <vendor|hub|user|local|unknown>] [--require-allowed-tools] [--max-file-bytes N] [--ignore-trust]"
        )),
    }
}

/// `cos agent skills usage <stats|record|path|clear>`
///
/// Read/write surface over the skill-invocation JSONL log
/// ([`crate::agent::skills::provenance::UsageStore`]). Lives at
/// `agent_skills_usage_path()` (typically
/// `<data_dir>/agent/skills-usage.jsonl`).
///
/// * `stats [<id>]` — aggregate over the whole log, optionally
///   filtered to one skill id. Returns per-skill totals + average
///   duration + success rate.
/// * `record <id> --duration-ms N [--ok|--error] [--by <caller>]` —
///   append one usage record. Useful for external runners (a skill
///   that wraps an external script) to participate in the same
///   tracking surface.
/// * `path` — print the JSONL log path so callers can `tail -f` it
///   or pipe into their own analysis tooling.
/// * `clear` — truncate the log. Refuses without `--yes` so a
///   mistyped command can't wipe weeks of telemetry.
/// `cos agent skills guard <id> [--provenance <p>] [--require-allowed-tools] [--max-file-bytes N] [--ignore-trust]`
///
/// Run [`crate::agent::skills::provenance::Guard`] against an
/// installed skill loaded by [`crate::agent::skills::loader::load_default`]
/// and report what the guard would say at invocation time. Useful
/// for operators reviewing whether a freshly-installed third-party
/// skill would actually be allowed to run.
///
/// `--provenance` overrides the default `Hub` (the strict path).
/// Accepts the lowercase forms of [`Provenance`]: vendor / hub /
/// user / local / unknown. Default is `hub` so the guard runs the
/// full check tree.
///
/// `--require-allowed-tools` flips
/// [`GuardConfig::require_allowed_tools`] on so a skill with no
/// declared `allowed_tools` is rejected.
///
/// `--max-file-bytes N` overrides the per-sibling-file size cap
/// (default 5 MiB). Useful to test what would happen with a tighter
/// policy.
///
/// `--ignore-trust` flips
/// [`GuardConfig::honour_provenance_trust`] off so even
/// `vendor`/`user` skills run the strict checks (lets you preview
/// the worst-case verdict for a vendored skill).
///
/// Output includes the resolved verdict (allow / deny / require
/// confirmation), the GuardConfig that produced it, and the
/// provenance used. Returns an error if the skill id isn't loaded.
fn skills_guard_cmd(args: &[String]) -> Result<Value, String> {
    let res = skills::loader::load_default();
    skills_guard_cmd_against(args, &res.skills)
}

/// Inner form of [`skills_guard_cmd`] that takes the already-loaded
/// skill map. Lets tests construct a fake skill in a tempdir without
/// touching the live data dir.
fn skills_guard_cmd_against(
    args: &[String],
    skills: &std::collections::BTreeMap<String, skills::loader::LoadedSkill>,
) -> Result<Value, String> {
    use crate::agent::skills::provenance::{Guard, GuardConfig, GuardOutcome, Provenance};

    let mut id: Option<String> = None;
    let mut provenance = Provenance::Hub;
    let mut cfg = GuardConfig::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--provenance" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--provenance needs a value".to_string())?;
                provenance = match raw.to_ascii_lowercase().as_str() {
                    "vendor" => Provenance::Vendor,
                    "hub" => Provenance::Hub,
                    "user" => Provenance::User,
                    "local" => Provenance::Local,
                    "unknown" => Provenance::Unknown,
                    other => {
                        return Err(format!(
                        "unknown provenance: {other}. try: vendor | hub | user | local | unknown"
                    ))
                    }
                };
                i += 2;
            }
            "--require-allowed-tools" => {
                cfg.require_allowed_tools = true;
                i += 1;
            }
            "--max-file-bytes" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--max-file-bytes needs a value".to_string())?;
                cfg.max_file_bytes = raw
                    .parse::<u64>()
                    .map_err(|e| format!("--max-file-bytes parse: {e}"))?;
                i += 2;
            }
            "--ignore-trust" => {
                cfg.honour_provenance_trust = false;
                i += 1;
            }
            other if id.is_none() && !other.starts_with("--") => {
                id = Some(other.to_string());
                i += 1;
            }
            other => return Err(format!("unknown skills guard flag: {other}")),
        }
    }

    let id = id.ok_or_else(|| "usage: cos agent skills guard <id>".to_string())?;

    let skill = skills
        .get(&id)
        .ok_or_else(|| format!("skill not loaded: {id}"))?;

    let guard = Guard::new(cfg.clone());
    let outcome = guard.check(skill, provenance);
    let (verdict, reason) = match outcome {
        GuardOutcome::Allow => ("allow", None),
        GuardOutcome::Deny { reason } => ("deny", Some(reason)),
        GuardOutcome::RequireConfirmation { reason } => ("require_confirmation", Some(reason)),
    };

    Ok(json!({
        "id": skill.id,
        "verdict": verdict,
        "reason": reason,
        "provenance": provenance.as_str(),
        "config": {
            "max_file_bytes": cfg.max_file_bytes,
            "require_allowed_tools": cfg.require_allowed_tools,
            "honour_provenance_trust": cfg.honour_provenance_trust,
        },
    }))
}

fn skills_usage_cmd(args: &[String]) -> Result<Value, String> {
    let path = crate::paths::agent_skills_usage_path();
    skills_usage_cmd_at(args, &path)
}

fn skills_usage_cmd_at(args: &[String], path: &std::path::Path) -> Result<Value, String> {
    use crate::agent::skills::provenance::{UsageRecord, UsageStore};
    use chrono::Utc;

    let store = UsageStore::new(path);
    let sub = args.first().map(|s| s.as_str()).unwrap_or("stats");
    match sub {
        "path" => Ok(json!({"path": path.display().to_string()})),
        "stats" | "" => {
            let agg = store.aggregate();
            let filter_id = args.get(1).filter(|s| !s.is_empty()).cloned();
            let entries: Vec<Value> = agg
                .iter()
                .filter(|(id, _)| {
                    filter_id
                        .as_deref()
                        .map(|f| f == id.as_str())
                        .unwrap_or(true)
                })
                .map(|(id, s)| {
                    json!({
                        "id": id,
                        "total": s.total,
                        "success": s.success,
                        "failure": s.failure,
                        "total_duration_ms": s.total_duration_ms,
                        "average_duration_ms": s.average_duration_ms(),
                        "success_rate": if s.total == 0 {
                            None
                        } else {
                            Some((s.success as f64) / (s.total as f64))
                        },
                    })
                })
                .collect();
            Ok(json!({
                "path": path.display().to_string(),
                "skill_count": entries.len(),
                "filter_id": filter_id,
                "skills": entries,
            }))
        }
        "record" => {
            let id = args
                .get(1)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    "usage: cos agent skills usage record <id> --duration-ms N [--ok|--error] [--by <caller>]"
                        .to_string()
                })?;
            let mut duration_ms: Option<u64> = None;
            let mut success = true;
            let mut invoked_by: Option<String> = None;
            let mut i = 2usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--duration-ms" => {
                        duration_ms = Some(parse_u64_arg(args.get(i + 1), "--duration-ms")?);
                        i += 2;
                    }
                    "--ok" => {
                        success = true;
                        i += 1;
                    }
                    "--error" | "--fail" => {
                        success = false;
                        i += 1;
                    }
                    "--by" => {
                        invoked_by = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| "--by needs a name".to_string())?,
                        );
                        i += 2;
                    }
                    other => {
                        return Err(format!("unknown flag for `usage record`: {other}"));
                    }
                }
            }
            let duration_ms = duration_ms.ok_or_else(|| {
                "--duration-ms is required for `usage record`".to_string()
            })?;
            let rec = UsageRecord {
                skill_id: id.clone(),
                timestamp: Utc::now().to_rfc3339(),
                success,
                duration_ms,
                invoked_by: invoked_by.clone(),
                resource_path: None,
            };
            store
                .record(&rec)
                .map_err(|e| format!("record failed: {e}"))?;
            Ok(json!({
                "ok": true,
                "id": id,
                "timestamp": rec.timestamp,
                "success": success,
                "duration_ms": duration_ms,
                "invoked_by": invoked_by,
                "path": path.display().to_string(),
            }))
        }
        "clear" => {
            let confirmed = args.iter().any(|a| a == "--yes");
            if !confirmed {
                return Err(
                    "refusing to clear usage log without --yes (would discard all per-skill telemetry)"
                        .to_string(),
                );
            }
            if path.exists() {
                std::fs::remove_file(path)
                    .map_err(|e| format!("clear {}: {e}", path.display()))?;
            }
            Ok(json!({
                "ok": true,
                "path": path.display().to_string(),
                "cleared": true,
            }))
        }
        other => Err(format!(
            "unknown usage subcommand: {other}. try: stats [<id>] | record <id> --duration-ms N [--ok|--error] [--by <caller>] | path | clear --yes"
        )),
    }
}

/// `cos agent skills hub <list|show|install> <owner/repo> [<id>] [--force]`
///
/// Talks to a GitHub Releases-based skills hub
/// ([`crate::agent::skills::hub`]). `list` fetches the catalogue
/// from the latest release of `<owner>/<repo>` and emits the
/// available skills. `show` resolves one skill by id and emits its
/// download metadata. `install` downloads the asset, validates the
/// catalogue-declared SHA-256, and hands the local zip off to the
/// existing [`crate::agent::skills::sync::install_from_archive`]
/// pipeline.
///
/// Auth: optional GitHub PAT from `$COS_HUB_TOKEN`, `$GITHUB_TOKEN`,
/// or `$GH_TOKEN` (in that order). The token is forwarded to both
/// the GitHub REST API call and the asset download — required for
/// private hubs and helpful even for public hubs to avoid
/// unauthenticated rate limits.
fn skills_hub_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::skills::hub::{HubConfig, SkillsHub};

    let sub = args.first().map(|s| s.as_str()).ok_or_else(|| {
        "usage: cos agent skills hub <list|show|install> <owner/repo> [<id>] [--force]".to_string()
    })?;

    let spec = args
        .get(1)
        .cloned()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!("usage: cos agent skills hub {sub} <owner/repo> [<id>] [--force]")
        })?;
    let (owner, repo) = parse_owner_repo(&spec)?;

    let token = std::env::var("COS_HUB_TOKEN")
        .ok()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .or_else(|| std::env::var("GH_TOKEN").ok())
        .filter(|t| !t.is_empty());

    let hub = SkillsHub::new(HubConfig::new(owner.clone(), repo.clone()).with_token(token.clone()));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    match sub {
        "list" => {
            let cat = runtime
                .block_on(hub.latest_catalogue())
                .map_err(|e| format!("hub list failed: {e}"))?;
            let entries: Vec<Value> = cat
                .skills
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "name": s.name,
                        "version": s.version,
                        "asset": s.asset,
                        "sha256": s.sha256,
                        "tags": s.tags,
                        "description": s.description,
                    })
                })
                .collect();
            Ok(json!({
                "owner": owner,
                "repo": repo,
                "release_tag": cat.release_tag,
                "schema": cat.schema,
                "count": entries.len(),
                "skills": entries,
            }))
        }
        "show" => {
            let id = args
                .get(2)
                .cloned()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    "usage: cos agent skills hub show <owner/repo> <id>".to_string()
                })?;
            let resolved = runtime
                .block_on(hub.resolve(&id))
                .map_err(|e| format!("hub resolve failed: {e}"))?
                .ok_or_else(|| format!("no skill '{id}' in hub {owner}/{repo}"))?;
            Ok(json!({
                "id": resolved.entry.id,
                "name": resolved.entry.name,
                "version": resolved.entry.version,
                "asset": resolved.entry.asset,
                "sha256": resolved.entry.sha256,
                "size": resolved.size,
                "download_url": resolved.download_url,
            }))
        }
        "install" => {
            let id = args
                .get(2)
                .cloned()
                .filter(|s| !s.is_empty() && !s.starts_with("--"))
                .ok_or_else(|| {
                    "usage: cos agent skills hub install <owner/repo> <id> [--force]"
                        .to_string()
                })?;
            let force = args.iter().any(|a| a == "--force" || a == "-f");

            let resolved = runtime
                .block_on(hub.resolve(&id))
                .map_err(|e| format!("hub resolve failed: {e}"))?
                .ok_or_else(|| format!("no skill '{id}' in hub {owner}/{repo}"))?;

            let auth_header_owned = token.as_ref().map(|t| ("Authorization".to_string(), format!("Bearer {t}")));
            let mut header_pairs: Vec<(&str, &str)> = Vec::new();
            if let Some((k, v)) = auth_header_owned.as_ref() {
                header_pairs.push((k.as_str(), v.as_str()));
            }
            let download_label = format!("hub:{}/{}/{}", owner, repo, resolved.entry.id);
            let opts = crate::engine_pkg::download::DownloadOpts {
                url: &resolved.download_url,
                headers: &header_pairs,
                expected_sha256: Some(resolved.entry.sha256.as_str()),
                label: &download_label,
            };
            let dl = runtime
                .block_on(crate::engine_pkg::download::stream_to_temp(&opts))
                .map_err(|e| format!("download failed: {e}"))?;

            let res = skills::sync::install_from_archive(dl.temp_file.path(), force)
                .map_err(|e| format!("install failed: {e}"))?;
            Ok(json!({
                "ok": true,
                "id": res.id,
                "hub_id": resolved.entry.id,
                "version": resolved.entry.version,
                "install_dir": res.install_dir.display().to_string(),
                "files_extracted": res.files_extracted,
                "bytes_on_disk": res.bytes_on_disk,
                "bytes_downloaded": dl.bytes,
                "sha256": dl.sha256_hex,
                "replaced_existing": res.replaced_existing,
            }))
        }
        other => Err(format!(
            "unknown hub subcommand: {other}. try: list <owner/repo> | show <owner/repo> <id> | install <owner/repo> <id> [--force]"
        )),
    }
}

fn parse_owner_repo(spec: &str) -> Result<(String, String), String> {
    let mut parts = spec.splitn(2, '/');
    let owner = parts.next().unwrap_or("").trim();
    let repo = parts.next().unwrap_or("").trim();
    if owner.is_empty() || repo.is_empty() {
        return Err(format!(
            "expected '<owner>/<repo>' (e.g. clawos/skills-hub), got '{spec}'"
        ));
    }
    Ok((owner.to_string(), repo.to_string()))
}

fn parse_u64_arg(value: Option<&String>, flag: &str) -> Result<u64, String> {
    let v = value.ok_or_else(|| format!("{flag} needs an integer"))?;
    v.parse::<u64>()
        .map_err(|e| format!("{flag}: invalid integer '{v}': {e}"))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/skills_commands.rs"
    ));
}
