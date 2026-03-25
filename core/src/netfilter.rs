/// OS-level outbound network firewall — domain-based allow/deny rules with rate limiting.
///
/// Analogous to iptables/nftables, this provides declarative network access
/// control for agent processes. Rules are persisted as JSON and enforced
/// at the sandbox level via iptables (Linux) or advisory-only on other platforms.
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
use std::fs;
use std::path::PathBuf;

use crate::policy::{self, OpType};

fn netfilter_dir() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("netfilter")
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

/// Add a firewall rule.
///
/// Usage: cos netfilter add --allow <domain> [--port N] [--method GET,POST] [--path "/api/**"] [--binary /usr/bin/git] [--tls]
///        cos netfilter add --deny <domain>
fn cmd_add(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

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

    let mut cfg = load_config();

    // Remove any existing rule for this exact domain+port+path+binary combo
    cfg.rules.retain(|r| {
        !(r.domain == domain && r.port == port && r.path == path && r.binary == binary)
    });
    cfg.rules.push(rule);
    save_config(&cfg);

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
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let domain = args.first().ok_or("usage: cos netfilter remove <domain>")?;
    let mut cfg = load_config();
    let before = cfg.rules.len();
    cfg.rules.retain(|r| r.domain != *domain);
    let removed = before - cfg.rules.len();
    save_config(&cfg);

    Ok(json!({
        "domain": domain,
        "removed": removed,
    }))
}

/// List all rules.
fn cmd_list(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

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
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

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
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let cfg = NetFilterConfig {
        default_policy: "allow-all".into(),
        rules: vec![],
        rate_limits: vec![],
    };
    save_config(&cfg);
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
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let policy_str = args
        .first()
        .ok_or("usage: cos netfilter default allow-all|deny-all")?;

    if policy_str != "allow-all" && policy_str != "deny-all" {
        return Err("default policy must be 'allow-all' or 'deny-all'".into());
    }

    let mut cfg = load_config();
    cfg.default_policy = policy_str.clone();
    save_config(&cfg);

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
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

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
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(state) = serde_json::from_str(&data) {
            return state;
        }
    }
    RateLimitState::default()
}

fn save_rate_state(state: &RateLimitState) {
    let dir = netfilter_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(data) = serde_json::to_string_pretty(state) {
        let _ = fs::write(rate_state_path(), data);
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
    policy::require(OpType::System).map_err(|v| v.to_string())?;

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

    let mut cfg = load_config();
    cfg.rate_limits.retain(|r| r.domain != *domain);
    cfg.rate_limits.push(rl);
    save_config(&cfg);

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
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

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
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let domain = args
        .first()
        .ok_or("usage: cos netfilter rate-limit-remove <domain>")?;

    let mut cfg = load_config();
    let before = cfg.rate_limits.len();
    cfg.rate_limits.retain(|r| r.domain != *domain);
    let removed = before - cfg.rate_limits.len();
    save_config(&cfg);

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
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let domain = args
        .first()
        .ok_or("usage: cos netfilter rate-check <domain> [--dry-run]")?;

    let dry_run = args.iter().any(|a| a == "--dry-run");

    let config = load_config();
    let rl = match find_rate_limit(&config, domain) {
        Some(rl) => rl,
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

    let mut state = load_rate_state();
    let timestamps = state
        .requests
        .get(domain.as_str())
        .cloned()
        .unwrap_or_default();

    // Prune old timestamps
    let active = prune_timestamps(&timestamps, 60);
    let count = active.len();
    let limit = rl.rpm + rl.burst;

    if count < limit as usize {
        // Allowed
        let remaining = limit as usize - count - 1; // -1 for this request
        if !dry_run {
            let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            let mut new_timestamps = active;
            new_timestamps.push(now);
            state.requests.insert(domain.clone(), new_timestamps);
            save_rate_state(&state);
        }
        Ok(json!({
            "domain": domain,
            "allowed": true,
            "requests_in_window": count,
            "limit": limit,
            "burst": rl.burst,
            "remaining": remaining,
        }))
    } else {
        // Denied
        let retry_after = earliest_expiry(&active, 60).unwrap_or(60);
        Ok(json!({
            "domain": domain,
            "allowed": false,
            "requests_in_window": count,
            "limit": limit,
            "burst": rl.burst,
            "remaining": 0,
            "retry_after_secs": retry_after,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, Once};

    static INIT: Once = Once::new();
    /// Netfilter tests must be serialized because they all write to the same
    /// rules.json file. Each test locks this mutex, resets rules, then runs.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire the lock, init the shared dir once, then reset rules.
    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap();
        INIT.call_once(|| {
            let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
            let _ = fs::create_dir_all(&dir);
            std::env::set_var("COS_DATA_DIR", &dir);
        });
        std::env::remove_var("COS_SESSION");
        let _ = cmd_reset(&vec![]);
        guard
    }

    #[test]
    fn domain_match_exact() {
        assert!(domain_matches("example.com", "example.com"));
        assert!(!domain_matches("example.com", "other.com"));
    }

    #[test]
    fn domain_match_wildcard() {
        assert!(domain_matches("*.example.com", "sub.example.com"));
        assert!(domain_matches("*.example.com", "example.com"));
        assert!(!domain_matches("*.example.com", "other.com"));
    }

    #[test]
    fn domain_match_star() {
        assert!(domain_matches("*", "anything.com"));
    }

    #[test]
    fn add_and_list_rules() {
        let _g = setup();
        cmd_add(&vec!["--allow".into(), "github.com".into()]).unwrap();
        cmd_add(&vec!["--deny".into(), "evil.com".into()]).unwrap();

        let r = cmd_list(&vec![]).unwrap();
        assert_eq!(r["count"], 2);
    }

    #[test]
    fn check_domain_with_rules() {
        let _g = setup();
        cmd_add(&vec!["--allow".into(), "api.openai.com".into()]).unwrap();
        cmd_add(&vec!["--deny".into(), "*.malware.com".into()]).unwrap();

        assert!(is_domain_allowed("api.openai.com"));
        assert!(!is_domain_allowed("sub.malware.com"));
    }

    #[test]
    fn deny_all_default() {
        let _g = setup();
        cmd_default(&vec!["deny-all".into()]).unwrap();
        cmd_add(&vec!["--allow".into(), "github.com".into()]).unwrap();

        assert!(is_domain_allowed("github.com"));
        assert!(!is_domain_allowed("random.com"));
    }

    #[test]
    fn remove_rule() {
        let _g = setup();
        cmd_add(&vec!["--allow".into(), "temp.com".into()]).unwrap();
        cmd_remove(&vec!["temp.com".into()]).unwrap();

        let r = cmd_list(&vec![]).unwrap();
        assert_eq!(r["count"], 0);
    }

    #[test]
    fn reset_clears_all() {
        let _g = setup();
        cmd_add(&vec!["--allow".into(), "a.com".into()]).unwrap();
        cmd_add(&vec!["--deny".into(), "b.com".into()]).unwrap();
        cmd_reset(&vec![]).unwrap();

        let r = cmd_list(&vec![]).unwrap();
        assert_eq!(r["count"], 0);
        assert_eq!(r["default_policy"], "allow-all");
    }

    #[test]
    fn run_dispatch() {
        let _g = setup();
        let r = run("add", &vec!["--allow".into(), "test.com".into()]).unwrap();
        assert_eq!(r["added"], true);

        let r = run("bogus", &vec![]);
        assert!(r.is_err());
    }

    // --- HTTP-level policy tests ---

    #[test]
    fn path_match_exact() {
        assert!(path_matches("/api/v1", "/api/v1"));
        assert!(!path_matches("/api/v1", "/api/v2"));
    }

    #[test]
    fn path_match_single_wildcard() {
        assert!(path_matches("/api/*", "/api/users"));
        assert!(!path_matches("/api/*", "/api/users/123"));
    }

    #[test]
    fn path_match_double_wildcard() {
        assert!(path_matches("/api/**", "/api/users"));
        assert!(path_matches("/api/**", "/api/users/123/posts"));
        assert!(path_matches("/**", "/anything/at/all"));
    }

    #[test]
    fn evaluate_with_method_filter() {
        let _g = setup();
        cmd_default(&vec!["deny-all".into()]).unwrap();
        cmd_add(&vec![
            "--allow".into(),
            "api.example.com".into(),
            "--method".into(),
            "GET,POST".into(),
        ])
        .unwrap();

        let r = evaluate("api.example.com", Some("GET"), None, None);
        assert!(r.allowed);

        let r = evaluate("api.example.com", Some("DELETE"), None, None);
        assert!(!r.allowed);
    }

    #[test]
    fn evaluate_with_path_filter() {
        let _g = setup();
        cmd_default(&vec!["deny-all".into()]).unwrap();
        cmd_add(&vec![
            "--allow".into(),
            "api.telegram.org".into(),
            "--path".into(),
            "/bot/**".into(),
        ])
        .unwrap();

        let r = evaluate("api.telegram.org", None, Some("/bot/sendMessage"), None);
        assert!(r.allowed);

        let r = evaluate("api.telegram.org", None, Some("/admin/delete"), None);
        assert!(!r.allowed);
    }

    #[test]
    fn evaluate_with_binary_filter() {
        let _g = setup();
        cmd_default(&vec!["deny-all".into()]).unwrap();
        cmd_add(&vec![
            "--allow".into(),
            "github.com".into(),
            "--binary".into(),
            "/usr/bin/git".into(),
        ])
        .unwrap();

        let r = evaluate("github.com", None, None, Some("/usr/bin/git"));
        assert!(r.allowed);

        let r = evaluate("github.com", None, None, Some("/usr/bin/curl"));
        assert!(!r.allowed);
    }

    #[test]
    fn evaluate_combined_filters() {
        let _g = setup();
        cmd_default(&vec!["deny-all".into()]).unwrap();
        cmd_add(&vec![
            "--allow".into(),
            "api.openai.com".into(),
            "--method".into(),
            "POST".into(),
            "--path".into(),
            "/v1/chat/**".into(),
        ])
        .unwrap();

        // POST to /v1/chat/completions — allowed
        let r = evaluate(
            "api.openai.com",
            Some("POST"),
            Some("/v1/chat/completions"),
            None,
        );
        assert!(r.allowed);

        // GET to /v1/chat/completions — denied (wrong method)
        let r = evaluate(
            "api.openai.com",
            Some("GET"),
            Some("/v1/chat/completions"),
            None,
        );
        assert!(!r.allowed);

        // POST to /v1/models — denied (wrong path)
        let r = evaluate("api.openai.com", Some("POST"), Some("/v1/models"), None);
        assert!(!r.allowed);
    }

    #[test]
    fn export_returns_full_config() {
        let _g = setup();
        cmd_add(&vec![
            "--allow".into(),
            "example.com".into(),
            "--method".into(),
            "GET".into(),
            "--path".into(),
            "/api/**".into(),
            "--tls".into(),
        ])
        .unwrap();

        let r = cmd_export(&vec![]).unwrap();
        assert_eq!(r["rules"][0]["domain"], "example.com");
        assert_eq!(r["rules"][0]["methods"][0], "GET");
        assert_eq!(r["rules"][0]["path"], "/api/**");
        assert_eq!(r["rules"][0]["tls_required"], true);
    }

    // --- Rate limiting tests ---

    #[test]
    fn test_rate_limit_add_and_list() {
        let _g = setup();
        let r = cmd_rate_limit(&vec![
            "api.openai.com".into(),
            "--rpm".into(),
            "60".into(),
            "--burst".into(),
            "10".into(),
        ])
        .unwrap();
        assert_eq!(r["domain"], "api.openai.com");
        assert_eq!(r["rpm"], 60);
        assert_eq!(r["burst"], 10);

        let r = cmd_rate_limits(&vec![]).unwrap();
        assert_eq!(r["count"], 1);
        assert_eq!(r["rate_limits"][0]["domain"], "api.openai.com");
        assert_eq!(r["rate_limits"][0]["rpm"], 60);
        assert_eq!(r["rate_limits"][0]["burst"], 10);
    }

    #[test]
    fn test_rate_limit_remove() {
        let _g = setup();
        cmd_rate_limit(&vec!["api.openai.com".into(), "--rpm".into(), "60".into()]).unwrap();

        let r = cmd_rate_limit_remove(&vec!["api.openai.com".into()]).unwrap();
        assert_eq!(r["removed"], 1);

        let r = cmd_rate_limits(&vec![]).unwrap();
        assert_eq!(r["count"], 0);
    }

    #[test]
    fn test_rate_check_allowed() {
        let _g = setup();
        cmd_rate_limit(&vec!["api.openai.com".into(), "--rpm".into(), "10".into()]).unwrap();

        for i in 0..5 {
            let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
            assert_eq!(r["allowed"], true, "request {i} should be allowed");
            assert_eq!(r["requests_in_window"], i);
        }
    }

    #[test]
    fn test_rate_check_denied() {
        let _g = setup();
        cmd_rate_limit(&vec!["api.openai.com".into(), "--rpm".into(), "3".into()]).unwrap();

        for _ in 0..3 {
            let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
            assert_eq!(r["allowed"], true);
        }

        // 4th request should be denied
        let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
        assert_eq!(r["allowed"], false);
        assert_eq!(r["remaining"], 0);
        assert!(r["retry_after_secs"].as_u64().unwrap() > 0);
    }

    #[test]
    fn test_rate_check_dry_run() {
        let _g = setup();
        cmd_rate_limit(&vec!["api.openai.com".into(), "--rpm".into(), "10".into()]).unwrap();

        // Dry run should not record
        let r = cmd_rate_check(&vec!["api.openai.com".into(), "--dry-run".into()]).unwrap();
        assert_eq!(r["allowed"], true);
        assert_eq!(r["requests_in_window"], 0);

        // Still 0 after dry run
        let r = cmd_rate_check(&vec!["api.openai.com".into(), "--dry-run".into()]).unwrap();
        assert_eq!(r["requests_in_window"], 0);

        // Real request records it
        let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
        assert_eq!(r["allowed"], true);
        assert_eq!(r["requests_in_window"], 0); // was 0 before this request

        // Now there's 1
        let r = cmd_rate_check(&vec!["api.openai.com".into(), "--dry-run".into()]).unwrap();
        assert_eq!(r["requests_in_window"], 1);
    }

    #[test]
    fn test_rate_check_window_cleanup() {
        let _g = setup();
        cmd_rate_limit(&vec!["api.openai.com".into(), "--rpm".into(), "10".into()]).unwrap();

        // Manually inject old timestamps that are outside the 60s window
        let old_time = (chrono::Utc::now() - chrono::Duration::seconds(120))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut state = RateLimitState::default();
        state
            .requests
            .insert("api.openai.com".into(), vec![old_time; 5]);
        save_rate_state(&state);

        // Old timestamps should be pruned — count should be 0
        let r = cmd_rate_check(&vec!["api.openai.com".into(), "--dry-run".into()]).unwrap();
        assert_eq!(r["allowed"], true);
        assert_eq!(r["requests_in_window"], 0);
    }

    #[test]
    fn test_find_rate_limit_wildcard() {
        let _g = setup();
        cmd_rate_limit(&vec!["*.openai.com".into(), "--rpm".into(), "30".into()]).unwrap();

        // Wildcard should match subdomains
        let config = load_config();
        let rl = find_rate_limit(&config, "api.openai.com");
        assert!(rl.is_some());
        assert_eq!(rl.unwrap().rpm, 30);

        // Should also match the base domain
        let rl = find_rate_limit(&config, "openai.com");
        assert!(rl.is_some());
    }

    #[test]
    fn test_rate_limit_burst() {
        let _g = setup();
        cmd_rate_limit(&vec![
            "api.openai.com".into(),
            "--rpm".into(),
            "2".into(),
            "--burst".into(),
            "1".into(),
        ])
        .unwrap();

        // rpm=2, burst=1 → total limit = 3
        for i in 0..3 {
            let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
            assert_eq!(
                r["allowed"], true,
                "request {i} should be allowed (limit=3)"
            );
        }

        // 4th request should be denied
        let r = cmd_rate_check(&vec!["api.openai.com".into()]).unwrap();
        assert_eq!(r["allowed"], false);
        assert_eq!(r["limit"], 3);
    }
}
