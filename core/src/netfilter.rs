/// OS-level outbound network firewall — domain-based allow/deny rules.
///
/// Analogous to iptables/nftables, this provides declarative network access
/// control for agent processes. Rules are persisted as JSON and enforced
/// at the sandbox level via iptables (Linux) or advisory-only on other platforms.
///
/// Storage: `$COS_DATA_DIR/netfilter/rules.json`
///
/// Commands:
///   add --allow <domain> [--port N]  — allow outbound to a domain
///   add --deny <domain>              — deny outbound to a domain
///   remove <domain>                  — remove a rule
///   list                             — list all rules
///   check <domain>                   — check if a domain is allowed
///   reset                            — remove all rules (allow-all default)
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NetFilterConfig {
    /// "allow-all" (default) or "deny-all"
    pub default_policy: String,
    pub rules: Vec<NetRule>,
}

fn load_config() -> NetFilterConfig {
    let path = rules_path();
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str(&data) {
            return cfg;
        }
    }
    NetFilterConfig {
        default_policy: "allow-all".into(),
        rules: vec![],
    }
}

fn save_config(cfg: &NetFilterConfig) {
    let dir = netfilter_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(data) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(rules_path(), data);
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

    Ok(json!({
        "domain": domain,
        "allowed": result.allowed,
        "matched_rule": result.matched_rule,
        "reason": result.reason,
    }))
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

/// Reset all rules.
fn cmd_reset(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let cfg = NetFilterConfig {
        default_policy: "allow-all".into(),
        rules: vec![],
    };
    save_config(&cfg);

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

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU32, Ordering};
    static NF_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn setup() {
        let n = NF_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("cos-netfilter-test-{}-{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("COS_DATA_DIR", &dir);
        std::env::remove_var("COS_SESSION");
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
        setup();
        cmd_add(&vec!["--allow".into(), "github.com".into()]).unwrap();
        cmd_add(&vec!["--deny".into(), "evil.com".into()]).unwrap();

        let r = cmd_list(&vec![]).unwrap();
        assert_eq!(r["count"], 2);
    }

    #[test]
    fn check_domain_with_rules() {
        setup();
        cmd_add(&vec!["--allow".into(), "api.openai.com".into()]).unwrap();
        cmd_add(&vec!["--deny".into(), "*.malware.com".into()]).unwrap();

        assert!(is_domain_allowed("api.openai.com"));
        assert!(!is_domain_allowed("sub.malware.com"));
    }

    #[test]
    fn deny_all_default() {
        setup();
        cmd_default(&vec!["deny-all".into()]).unwrap();
        cmd_add(&vec!["--allow".into(), "github.com".into()]).unwrap();

        assert!(is_domain_allowed("github.com"));
        assert!(!is_domain_allowed("random.com"));
    }

    #[test]
    fn remove_rule() {
        setup();
        cmd_add(&vec!["--allow".into(), "temp.com".into()]).unwrap();
        cmd_remove(&vec!["temp.com".into()]).unwrap();

        let r = cmd_list(&vec![]).unwrap();
        assert_eq!(r["count"], 0);
    }

    #[test]
    fn reset_clears_all() {
        setup();
        cmd_add(&vec!["--allow".into(), "a.com".into()]).unwrap();
        cmd_add(&vec!["--deny".into(), "b.com".into()]).unwrap();
        cmd_reset(&vec![]).unwrap();

        let r = cmd_list(&vec![]).unwrap();
        assert_eq!(r["count"], 0);
        assert_eq!(r["default_policy"], "allow-all");
    }

    #[test]
    fn run_dispatch() {
        setup();
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
        setup();
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
        setup();
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
        setup();
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
        setup();
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
        setup();
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
}
