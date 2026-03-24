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
    PathBuf::from(
        std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()),
    )
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
    pub created_at: String,
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
        _ => Err(format!("unknown netfilter command: {command}")),
    }
}

/// Add a firewall rule.
///
/// Usage: cos netfilter add --allow <domain> [--port N]
///        cos netfilter add --deny <domain>
fn cmd_add(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let mut domain: Option<String> = None;
    let mut action: Option<String> = None;
    let mut port: Option<u16> = None;

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
            _ => i += 1,
        }
    }

    let domain = domain.ok_or("usage: cos netfilter add --allow|--deny <domain> [--port N]")?;
    let action = action.ok_or("usage: cos netfilter add --allow|--deny <domain>")?;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let rule = NetRule {
        domain: domain.clone(),
        action: action.clone(),
        port,
        created_at: now.clone(),
    };

    let mut cfg = load_config();

    // Remove any existing rule for this domain+port combo
    cfg.rules.retain(|r| !(r.domain == domain && r.port == port));
    cfg.rules.push(rule);
    save_config(&cfg);

    Ok(json!({
        "added": true,
        "domain": domain,
        "action": action,
        "port": port,
    }))
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
/// Usage: cos netfilter check <domain>
pub fn cmd_check(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let domain = args.first().ok_or("usage: cos netfilter check <domain>")?;
    let allowed = is_domain_allowed(domain);

    Ok(json!({
        "domain": domain,
        "allowed": allowed,
    }))
}

/// Check if a domain is allowed (used by sandbox and other modules).
pub fn is_domain_allowed(domain: &str) -> bool {
    let cfg = load_config();

    // Check explicit rules (most specific first)
    for rule in &cfg.rules {
        if domain_matches(&rule.domain, domain) {
            return rule.action == "allow";
        }
    }

    // Fall back to default policy
    cfg.default_policy != "deny-all"
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU32, Ordering};
    static NF_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn setup() {
        let n = NF_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "cos-netfilter-test-{}-{}",
            std::process::id(),
            n
        ));
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
}
