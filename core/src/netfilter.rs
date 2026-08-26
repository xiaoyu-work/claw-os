/// OS-level outbound network policy engine — domain-based allow/deny rules
/// with rate limiting.
///
/// This module declares network access policy. Rules are evaluated by
/// `check` and `rate-check` commands. Enforcement is the responsibility
/// of the network proxy or sandbox layer — this module does not modify
/// iptables/nftables directly.
///
/// For full enforcement, combine with:
///   - An HTTP proxy that calls `cos netfilter check` before forwarding
///   - The agent's sandbox tool, which can run subprocesses with
///     `--no-network` for complete isolation (see
///     `crate::agent::tools::cos_proxy::cos_sandbox`)
///
/// Storage: `$COS_DATA_DIR/netfilter/rules.json`
///            `$COS_DATA_DIR/netfilter/rate-state.json`
///
/// Commands:
///   add --allow <domain> [--port N]  — allow outbound to a domain
///   add --deny <domain>              — deny outbound to a domain
///   remove <domain>                  — remove a rule
///   list                             — list all rules
///   check <domain>                   — check if a domain is allowed
///   reset                            — remove all rules (allow-all default)
///   rate-limit <domain> --rpm N [--burst N] — set rate limit for a domain
///   rate-limits                       — list all rate limits
///   rate-limit-remove <domain>        — remove a rate limit
///   rate-check <domain> [--dry-run]   — check/record a request against rate limits
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::caps::{require_or_json, Scope, Verb};

fn netfilter_dir() -> PathBuf {
    crate::paths::data_dir().join("netfilter")
}

fn rules_path() -> PathBuf {
    netfilter_dir().join("rules.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetRule {
    pub domain: String,
    pub action: String, // "allow" or "deny"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// HTTP methods allowed (e.g., ["GET", "POST"]). Empty = all methods.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<String>,
    /// URL path pattern (e.g., "/api/**", "/bot*/**"). Empty = all paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Binary allowed to access this endpoint (e.g., "/usr/bin/git"). Empty = any binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// Require TLS for this rule.
    #[serde(default, skip_serializing_if = "is_false")]
    pub tls_required: bool,
    pub created_at: String,
}

fn is_false(v: &bool) -> bool {
    !v
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimit {
    pub domain: String,
    pub rpm: u32,
    #[serde(default)]
    pub burst: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RateLimitState {
    /// domain -> list of request timestamps (ISO 8601)
    requests: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetFilterConfig {
    /// "allow-all" (default) or "deny-all"
    pub default_policy: String,
    pub rules: Vec<NetRule>,
    #[serde(default)]
    pub rate_limits: Vec<RateLimit>,
}

fn load_config() -> NetFilterConfig {
    match crate::filelock::read_locked(&rules_path()) {
        Ok(Some(data)) => serde_json::from_str(&data).unwrap_or(NetFilterConfig {
            default_policy: "allow-all".into(),
            rules: vec![],
            rate_limits: vec![],
        }),
        _ => NetFilterConfig {
            default_policy: "allow-all".into(),
            rules: vec![],
            rate_limits: vec![],
        },
    }
}

fn save_config(cfg: &NetFilterConfig) {
    if let Ok(data) = serde_json::to_string_pretty(cfg) {
        let _ = crate::filelock::write_locked(&rules_path(), &data);
    }
}

/// Atomic read-modify-write on the netfilter config. Without this,
/// `load_config()` + mutate + `save_config()` is a textbook lost-
/// update race: two concurrent `cos netfilter add` calls both read
/// the same starting state and the second `save_config` clobbers
/// the first. Funnel every mutation through here.
fn update_config<F>(transform: F) -> Result<(), String>
where
    F: FnOnce(NetFilterConfig) -> NetFilterConfig,
{
    crate::filelock::update_locked::<_, String>(&rules_path(), |existing| {
        let cfg = match existing {
            Some(s) => serde_json::from_str::<NetFilterConfig>(&s).unwrap_or(NetFilterConfig {
                default_policy: "allow-all".into(),
                rules: vec![],
                rate_limits: vec![],
            }),
            None => NetFilterConfig {
                default_policy: "allow-all".into(),
                rules: vec![],
                rate_limits: vec![],
            },
        };
        let next = transform(cfg);
        serde_json::to_string_pretty(&next).map_err(|e| format!("serialize: {e}"))
    })
    .map_err(|e| e.to_string())
}

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "add" => cmd_add(args),
        "remove" => cmd_remove(args),
        "list" => cmd_list(args),
        "check" => cmd_check(args),
        "reset" => cmd_reset(args),
        "default" => cmd_default(args),
        "export" => cmd_export(args),
        "rate-limit" => cmd_rate_limit(args),
        "rate-limits" => cmd_rate_limits(args),
        "rate-limit-remove" => cmd_rate_limit_remove(args),
        "rate-check" => cmd_rate_check(args),
        _ => Err(format!("unknown netfilter command: {command}")),
    }
}

/// Add a network policy rule.
///
/// Usage: cos netfilter add --allow <domain> [--port N] [--method GET,POST] [--path "/api/**"] [--binary /usr/bin/git] [--tls]
///        cos netfilter add --deny <domain>
fn cmd_add(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let mut domain: Option<String> = None;
    let mut action: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut methods: Vec<String> = Vec::new();
    let mut path: Option<String> = None;
    let mut binary: Option<String> = None;
    let mut tls_required = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--allow" if i + 1 < args.len() => {
                action = Some("allow".into());
                domain = Some(args[i + 1].clone());
                i += 2;
            }
            "--deny" if i + 1 < args.len() => {
                action = Some("deny".into());
                domain = Some(args[i + 1].clone());
                i += 2;
            }
            "--port" if i + 1 < args.len() => {
                port = Some(
                    args[i + 1]
                        .parse::<u16>()
                        .map_err(|_| format!("invalid port: {}", args[i + 1]))?,
                );
                i += 2;
            }
            "--method" if i + 1 < args.len() => {
                methods = args[i + 1]
                    .split(',')
                    .map(|m| m.trim().to_uppercase())
                    .collect();
                i += 2;
            }
            "--path" if i + 1 < args.len() => {
                path = Some(args[i + 1].clone());
                i += 2;
            }
            "--binary" if i + 1 < args.len() => {
                binary = Some(args[i + 1].clone());
                i += 2;
            }
            "--tls" => {
                tls_required = true;
                i += 1;
            }
            _ => i += 1,
        }
    }

    let domain = domain.ok_or("usage: cos netfilter add --allow|--deny <domain> [--port N] [--method GET,POST] [--path \"/api/**\"] [--binary /usr/bin/git] [--tls]")?;
    let action = action.ok_or("usage: cos netfilter add --allow|--deny <domain>")?;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let rule = NetRule {
        domain: domain.clone(),
        action: action.clone(),
        port,
        methods: methods.clone(),
        path: path.clone(),
        binary: binary.clone(),
        tls_required,
        created_at: now,
    };

    let domain_for_retain = domain.clone();
    let port_for_retain = port;
    let path_for_retain = path.clone();
    let binary_for_retain = binary.clone();
    let rule_for_push = rule;
    update_config(|mut cfg| {
        cfg.rules.retain(|r| {
            !(r.domain == domain_for_retain
                && r.port == port_for_retain
                && r.path == path_for_retain
                && r.binary == binary_for_retain)
        });
        cfg.rules.push(rule_for_push);
        cfg
    })?;

    let mut result = json!({
        "added": true,
        "domain": domain,
        "action": action,
    });
    if let Some(p) = port {
        result["port"] = json!(p);
    }
    if !methods.is_empty() {
        result["methods"] = json!(methods);
    }
    if let Some(ref p) = path {
        result["path"] = json!(p);
    }
    if let Some(ref b) = binary {
        result["binary"] = json!(b);
    }
    if tls_required {
        result["tls_required"] = json!(true);
    }
    Ok(result)
}

/// Remove a rule by domain.
///
/// Usage: cos netfilter remove <domain>
fn cmd_remove(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let domain = args.first().ok_or("usage: cos netfilter remove <domain>")?;
    let domain_clone = domain.clone();
    let mut removed = 0_usize;
    let removed_ref = &mut removed;
    update_config(|mut cfg| {
        let before = cfg.rules.len();
        cfg.rules.retain(|r| r.domain != domain_clone);
        *removed_ref = before - cfg.rules.len();
        cfg
    })?;

    Ok(json!({
        "domain": domain,
        "removed": removed,
    }))
}

/// List all rules.
fn cmd_list(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let cfg = load_config();
    let rules: Vec<Value> = cfg
        .rules
        .iter()
        .map(|r| {
            let mut v = json!({
                "domain": r.domain,
                "action": r.action,
                "created_at": r.created_at,
            });
            if let Some(port) = r.port {
                v["port"] = json!(port);
            }
            if !r.methods.is_empty() {
                v["methods"] = json!(r.methods);
            }
            if let Some(ref path) = r.path {
                v["path"] = json!(path);
            }
            if let Some(ref binary) = r.binary {
                v["binary"] = json!(binary);
            }
            if r.tls_required {
                v["tls_required"] = json!(true);
            }
            v
        })
        .collect();

    Ok(json!({
        "default_policy": cfg.default_policy,
        "rules": rules,
        "count": rules.len(),
    }))
}

/// Check if a domain is allowed under current rules.
///
/// Usage: cos netfilter check <domain> [--method GET] [--path /api/v1] [--binary /usr/bin/curl]
pub fn cmd_check(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let domain = args
        .first()
        .ok_or("usage: cos netfilter check <domain> [--method M] [--path P] [--binary B]")?;

    let mut method: Option<String> = None;
    let mut path: Option<String> = None;
    let mut binary: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--method" if i + 1 < args.len() => {
                method = Some(args[i + 1].to_uppercase());
                i += 2;
            }
            "--path" if i + 1 < args.len() => {
                path = Some(args[i + 1].clone());
                i += 2;
            }
            "--binary" if i + 1 < args.len() => {
                binary = Some(args[i + 1].clone());
                i += 2;
            }
            _ => i += 1,
        }
    }

    let result = evaluate(
        domain,
        method.as_deref(),
        path.as_deref(),
        binary.as_deref(),
    );

    let mut out = json!({
        "domain": domain,
        "allowed": result.allowed,
        "matched_rule": result.matched_rule,
        "reason": result.reason,
    });

    // Also check rate limits if the domain is allowed
    if result.allowed {
        let config = load_config();
        if let Some(rl) = find_rate_limit(&config, domain) {
            let state = load_rate_state();
            let timestamps = state.requests.get(domain).cloned().unwrap_or_default();
            let count = count_requests_in_window(&timestamps, 60);
            let limit = rl.rpm + rl.burst;
            if count >= limit as usize {
                out["rate_limited"] = json!(true);
                out["requests_in_window"] = json!(count);
                out["limit"] = json!(limit);
            }
        }
    }

    Ok(out)
}

/// Result of a network policy evaluation.
pub struct EvalResult {
    pub allowed: bool,
    pub matched_rule: Option<String>,
    pub reason: String,
}

/// Evaluate a request against netfilter rules (used by proxy integrations).
///
/// Checks domain, method, path, and binary against all rules.
/// Returns detailed result for audit/logging.
pub fn evaluate(
    domain: &str,
    method: Option<&str>,
    path: Option<&str>,
    binary: Option<&str>,
) -> EvalResult {
    let cfg = load_config();

    for rule in &cfg.rules {
        if !domain_matches(&rule.domain, domain) {
            continue;
        }

        // Check method filter
        if !rule.methods.is_empty() {
            if let Some(m) = method {
                if !rule.methods.iter().any(|rm| rm == m) {
                    continue;
                }
            }
        }

        // Check path filter
        if let Some(ref rule_path) = rule.path {
            if let Some(req_path) = path {
                if !path_matches(rule_path, req_path) {
                    continue;
                }
            }
        }

        // Check binary filter
        if let Some(ref rule_bin) = rule.binary {
            if let Some(req_bin) = binary {
                if rule_bin != req_bin {
                    continue;
                }
            }
        }

        let allowed = rule.action == "allow";
        return EvalResult {
            allowed,
            matched_rule: Some(rule.domain.clone()),
            reason: format!("matched rule: {} {}", rule.action, rule.domain),
        };
    }

    // Fall back to default policy
    let allowed = cfg.default_policy != "deny-all";
    EvalResult {
        allowed,
        matched_rule: None,
        reason: format!("default policy: {}", cfg.default_policy),
    }
}

/// Check if a domain is allowed (simple check, backward compatible).
pub fn is_domain_allowed(domain: &str) -> bool {
    evaluate(domain, None, None, None).allowed
}

/// Simple path matching with glob-like wildcards.
/// Supports: /exact, /prefix/*, /prefix/**
fn path_matches(pattern: &str, path: &str) -> bool {
    if pattern == "/**" || pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        if !path.starts_with(&format!("{prefix}/")) {
            return false;
        }
        // Single level: no more slashes after prefix
        let rest = &path[prefix.len() + 1..];
        return !rest.contains('/');
    }
    path == pattern
}

/// Simple domain matching: supports exact match and wildcard prefix (*.example.com).
fn domain_matches(pattern: &str, domain: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        // *.example.com matches example.com and sub.example.com
        return domain == suffix || domain.ends_with(&format!(".{suffix}"));
    }
    domain == pattern
}

/// Reset all rules and rate limits.
fn cmd_reset(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    update_config(|_| NetFilterConfig {
        default_policy: "allow-all".into(),
        rules: vec![],
        rate_limits: vec![],
    })?;
    save_rate_state(&RateLimitState::default());

    Ok(json!({
        "reset": true,
        "default_policy": "allow-all",
    }))
}

/// Set default policy.
///
/// Usage: cos netfilter default allow-all|deny-all
fn cmd_default(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let policy_str = args
        .first()
        .ok_or("usage: cos netfilter default allow-all|deny-all")?;

    if policy_str != "allow-all" && policy_str != "deny-all" {
        return Err("default policy must be 'allow-all' or 'deny-all'".into());
    }

    let policy_clone = policy_str.clone();
    update_config(|mut cfg| {
        cfg.default_policy = policy_clone;
        cfg
    })?;

    Ok(json!({
        "default_policy": policy_str,
    }))
}

/// Export rules as a proxy-consumable JSON document.
///
/// Usage: cos netfilter export
///
/// Returns the full config including all HTTP-level fields,
/// suitable for consumption by an external proxy (mitmproxy, squid, nginx, etc.).
fn cmd_export(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let cfg = load_config();
    serde_json::to_value(&cfg).map_err(|e| format!("failed to serialize config: {e}"))
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

fn rate_state_path() -> PathBuf {
    netfilter_dir().join("rate-state.json")
}

fn load_rate_state() -> RateLimitState {
    let path = rate_state_path();
    crate::filelock::read_locked(&path)
        .ok()
        .and_then(|data| data)
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
}

fn save_rate_state(state: &RateLimitState) {
    let path = rate_state_path();
    if let Ok(data) = serde_json::to_string_pretty(state) {
        let _ = crate::filelock::write_locked(&path, &data);
    }
}

/// Find the rate limit for a domain. Exact match first, then wildcard.
fn find_rate_limit<'a>(config: &'a NetFilterConfig, domain: &str) -> Option<&'a RateLimit> {
    // Exact match first
    if let Some(rl) = config.rate_limits.iter().find(|rl| rl.domain == domain) {
        return Some(rl);
    }
    // Wildcard match (*.example.com)
    config
        .rate_limits
        .iter()
        .find(|rl| rl.domain != domain && domain_matches(&rl.domain, domain))
}

/// Count timestamps within the last `window_secs` seconds.
fn count_requests_in_window(timestamps: &[String], window_secs: u64) -> usize {
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(window_secs as i64);
    timestamps
        .iter()
        .filter(|ts| {
            chrono::DateTime::parse_from_rfc3339(ts)
                .map(|t| t >= cutoff)
                .unwrap_or(false)
        })
        .count()
}

/// Return timestamps that are still within the window (pruned).
fn prune_timestamps(timestamps: &[String], window_secs: u64) -> Vec<String> {
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(window_secs as i64);
    timestamps
        .iter()
        .filter(|ts| {
            chrono::DateTime::parse_from_rfc3339(ts)
                .map(|t| t >= cutoff)
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Return seconds until the oldest request in the window expires.
/// This is the "retry_after_secs" value.
fn earliest_expiry(timestamps: &[String], window_secs: u64) -> Option<u64> {
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::seconds(window_secs as i64);

    timestamps
        .iter()
        .filter_map(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
        .filter(|t| *t >= cutoff)
        .min()
        .map(|oldest| {
            let expires_at = oldest + chrono::Duration::seconds(window_secs as i64);
            let diff = expires_at.signed_duration_since(now);
            if diff.num_seconds() > 0 {
                diff.num_seconds() as u64
            } else {
                0
            }
        })
}

/// Set a rate limit for a domain.
///
/// Usage: cos netfilter rate-limit <domain> --rpm N [--burst N]
fn cmd_rate_limit(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let domain = args
        .first()
        .ok_or("usage: cos netfilter rate-limit <domain> --rpm N [--burst N]")?;

    let mut rpm: Option<u32> = None;
    let mut burst: u32 = 0;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--rpm" if i + 1 < args.len() => {
                rpm = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid rpm: {}", args[i + 1]))?,
                );
                i += 2;
            }
            "--burst" if i + 1 < args.len() => {
                burst = args[i + 1]
                    .parse::<u32>()
                    .map_err(|_| format!("invalid burst: {}", args[i + 1]))?;
                i += 2;
            }
            _ => i += 1,
        }
    }

    let rpm = rpm.ok_or("usage: cos netfilter rate-limit <domain> --rpm N [--burst N]")?;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let rl = RateLimit {
        domain: domain.clone(),
        rpm,
        burst,
        created_at: now,
    };

    let domain_for_rl = domain.clone();
    let rl_for_push = rl;
    update_config(|mut cfg| {
        cfg.rate_limits.retain(|r| r.domain != domain_for_rl);
        cfg.rate_limits.push(rl_for_push);
        cfg
    })?;

    Ok(json!({
        "domain": domain,
        "rpm": rpm,
        "burst": burst,
    }))
}

/// List all rate limits.
///
/// Usage: cos netfilter rate-limits
fn cmd_rate_limits(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let cfg = load_config();
    let limits: Vec<Value> = cfg
        .rate_limits
        .iter()
        .map(|rl| {
            json!({
                "domain": rl.domain,
                "rpm": rl.rpm,
                "burst": rl.burst,
                "created_at": rl.created_at,
            })
        })
        .collect();

    Ok(json!({
        "rate_limits": limits,
        "count": limits.len(),
    }))
}

/// Remove a rate limit for a domain.
///
/// Usage: cos netfilter rate-limit-remove <domain>
fn cmd_rate_limit_remove(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let domain = args
        .first()
        .ok_or("usage: cos netfilter rate-limit-remove <domain>")?;

    let domain_clone = domain.clone();
    let mut removed = 0_usize;
    let removed_ref = &mut removed;
    update_config(|mut cfg| {
        let before = cfg.rate_limits.len();
        cfg.rate_limits.retain(|r| r.domain != domain_clone);
        *removed_ref = before - cfg.rate_limits.len();
        cfg
    })?;

    // Also clean up state for this domain
    let mut state = load_rate_state();
    state.requests.remove(domain.as_str());
    save_rate_state(&state);

    Ok(json!({
        "domain": domain,
        "removed": removed,
    }))
}

/// Check if a request would be allowed under rate limits (and record it).
///
/// Usage: cos netfilter rate-check <domain> [--dry-run]
fn cmd_rate_check(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let domain = args
        .first()
        .ok_or("usage: cos netfilter rate-check <domain> [--dry-run]")?;

    let dry_run = args.iter().any(|a| a == "--dry-run");

    let config = load_config();
    let rl = match find_rate_limit(&config, domain) {
        Some(rl) => rl.clone(),
        None => {
            // No rate limit configured — always allowed
            return Ok(json!({
                "domain": domain,
                "allowed": true,
                "requests_in_window": 0,
                "limit": null,
                "burst": 0,
                "remaining": null,
            }));
        }
    };
    let limit = rl.rpm + rl.burst;

    // Counter increment must run inside a single locked window or
    // every concurrent caller reads the same counter, decides it's
    // under-limit, and writes its own +1 back — the rate limiter
    // becomes effectively disabled. Use update_locked to serialize
    // the entire read-prune-decide-write block.
    let path = rate_state_path();
    let mut decision: Option<(bool, usize, Option<u64>)> = None;
    let decision_ref = &mut decision;
    let domain_str = domain.clone();
    let result = crate::filelock::update_locked::<_, String>(&path, |existing| {
        let mut state: RateLimitState = match existing {
            Some(s) => serde_json::from_str(&s).unwrap_or_default(),
            None => RateLimitState::default(),
        };
        let timestamps = state
            .requests
            .get(domain_str.as_str())
            .cloned()
            .unwrap_or_default();
        let active = prune_timestamps(&timestamps, 60);
        let count = active.len();
        let allowed = count < limit as usize;
        let retry_after = if allowed {
            None
        } else {
            Some(earliest_expiry(&active, 60).unwrap_or(60))
        };

        let new_active = if allowed && !dry_run {
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let mut v = active;
            v.push(now);
            v
        } else {
            active
        };
        state.requests.insert(domain_str.clone(), new_active);

        *decision_ref = Some((allowed, count, retry_after));
        serde_json::to_string_pretty(&state).map_err(|e| format!("serialize: {e}"))
    });
    result.map_err(|e| e.to_string())?;

    let (allowed, count, retry_after) =
        decision.ok_or_else(|| "rate-check: missing decision (unreachable)".to_string())?;
    if allowed {
        let remaining = (limit as usize).saturating_sub(count).saturating_sub(1);
        Ok(json!({
            "domain": domain,
            "allowed": true,
            "requests_in_window": count,
            "limit": limit,
            "burst": rl.burst,
            "remaining": remaining,
        }))
    } else {
        Ok(json!({
            "domain": domain,
            "allowed": false,
            "requests_in_window": count,
            "limit": limit,
            "burst": rl.burst,
            "remaining": 0,
            "retry_after_secs": retry_after.unwrap_or(60),
        }))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/netfilter.rs"
    ));
}
