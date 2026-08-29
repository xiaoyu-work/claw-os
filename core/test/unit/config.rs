use super::*;

fn current_override() -> Option<&'static CosConfig> {
    CONFIG_OVERRIDE.try_with(|c| *c).ok()
}

#[test]
fn default_config_has_sensible_values() {
    let cfg = CosConfig::default();
    assert_eq!(cfg.version, env!("CARGO_PKG_VERSION"));
    // home defaults to $HOME at runtime, falling back to /root when unset.
    let expected_home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    assert_eq!(cfg.home, expected_home);
    assert_eq!(cfg.exec.timeout, 300);
    assert_eq!(cfg.exec.shell, "/bin/bash");
    assert_eq!(cfg.net.timeout, 30);
    assert!(cfg.net.allow_outbound);
    assert_eq!(cfg.web.engine, "cos-browser");
    assert_eq!(cfg.web.cdp_port, 9222);
    assert_eq!(cfg.web.max_content_length, 50000);
    assert_eq!(
        cfg.agent.tool_schema_budget_tokens,
        crate::agent::tools::progressive::DEFAULT_TOOL_SCHEMA_BUDGET_TOKENS
    );
}

#[test]
fn parse_partial_config() {
    let json = r#"{"version": "1.0.0", "home": "/custom"}"#;
    let cfg: CosConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.version, "1.0.0");
    assert_eq!(cfg.home, "/custom");
    // Defaults for missing sections
    assert_eq!(cfg.exec.timeout, 300);
    assert_eq!(cfg.web.engine, "cos-browser");
    assert_eq!(cfg.web.cdp_port, 9222);
}

#[test]
fn parse_extension_tool_schema_budget() {
    let cfg: CosConfig =
        serde_json::from_str(r#"{"agent":{"tool_schema_budget_tokens":0}}"#).unwrap();
    assert_eq!(cfg.agent.tool_schema_budget_tokens, 0);
}

#[test]
fn provider_fallback_overrides_provider_specific_fields() {
    let mut base = AgentConfig::default();
    base.provider = "anthropic".to_string();
    base.model = "primary-model".to_string();
    base.api_key_env = Some("ANTHROPIC_API_KEY".to_string());
    base.base_url = Some("https://primary.invalid".to_string());
    base.aws_region = Some("us-east-1".to_string());
    base.provider_fallbacks = vec![ProviderFallbackConfig {
        provider: "openai".to_string(),
        model: "fallback-model".to_string(),
        api_key_credential: None,
        api_key_env: Some("OPENAI_API_KEY".to_string()),
        api_key_credentials: Vec::new(),
        api_key_envs: Vec::new(),
        base_url: None,
        extra_headers: HashMap::new(),
        request_timeout: Some(45),
        pool_strategy: None,
        pool_cooldown_secs: None,
        ..Default::default()
    }];
    let fallback = base.provider_fallbacks[0].apply_to(&base);
    assert_eq!(fallback.provider, "openai");
    assert_eq!(fallback.model, "fallback-model");
    assert_eq!(fallback.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
    assert_eq!(fallback.base_url, None);
    assert_eq!(fallback.request_timeout, 45);
    assert_eq!(fallback.aws_region, None);
    assert!(fallback.provider_fallbacks.is_empty());
}

#[test]
fn parse_full_config() {
    let json = r#"{
        "version": "0.1.0",
        "home": "/home/cos",
        "exec": {"timeout": 600, "shell": "/bin/zsh"},
        "net": {"timeout": 10, "allow_outbound": false},
        "web": {"engine": "cos-browser", "cdp_port": 9333, "timeout": 60, "max_content_length": 100000}
    }"#;
    let cfg: CosConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.exec.timeout, 600);
    assert_eq!(cfg.exec.shell, "/bin/zsh");
    assert_eq!(cfg.net.timeout, 10);
    assert!(!cfg.net.allow_outbound);
    assert_eq!(cfg.web.cdp_port, 9333);
    assert_eq!(cfg.web.max_content_length, 100000);
}

#[test]
fn as_env_vars_returns_all_keys() {
    let vars = as_env_vars();
    let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
    assert!(keys.contains(&"COS_EXEC_TIMEOUT"));
    assert!(keys.contains(&"COS_NET_TIMEOUT"));
    assert!(keys.contains(&"COS_WEB_ENGINE"));
    assert!(keys.contains(&"COS_BROWSER_PORT"));
    assert!(keys.contains(&"COS_HOME"));
}

#[test]
fn malformed_json_returns_defaults() {
    let json = "not valid json {{{";
    let cfg: CosConfig = serde_json::from_str(json).unwrap_or_default();
    assert_eq!(cfg.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(cfg.exec.timeout, 300);
}

/// Parse-failure regression: a malformed config file on disk must
/// (a) NOT panic, (b) return the safe defaults so cos can still
/// boot, and (c) emit a tracing::error! so the operator notices.
/// The earlier behaviour swallowed the serde error with
/// `unwrap_or_default()` and silently downgraded the running
/// process to defaults — confusing because it looks like setup
/// was simply never done.
///
/// We can't easily intercept the tracing dispatcher inside a
/// unit test without adding test infrastructure, so this test
/// verifies (a) and (b) directly; the tracing::error! line is
/// also covered by the source-level audit / code-review path.
// regression: load_from_disk must tracing::error! on parse failure
#[test]
fn load_from_disk_returns_defaults_on_malformed_file_without_panic() {
    use std::io::Write;
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("cos-config-malformed-{pid}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.json");

    let mut f = fs::File::create(&path).unwrap();
    write!(f, "{{ this is definitely not json").unwrap();
    drop(f);

    let prev = std::env::var_os("COS_CONFIG_PATH");
    std::env::set_var("COS_CONFIG_PATH", &path);
    let cfg = load_from_disk();
    match prev {
        Some(x) => std::env::set_var("COS_CONFIG_PATH", x),
        None => std::env::remove_var("COS_CONFIG_PATH"),
    }
    let _ = fs::remove_dir_all(&dir);

    // Defaults: cos can still boot.
    assert_eq!(cfg.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(cfg.exec.timeout, 300);
    assert_eq!(cfg.exec.shell, "/bin/bash");
}

/// Missing-file regression: a fresh install with no config.json
/// must return defaults silently (no error log). This is the only
/// path through `load_from_disk` that does NOT log — make sure we
/// preserve that.
#[test]
fn load_from_disk_returns_defaults_when_file_missing() {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("cos-config-missing-{pid}"));
    let _ = fs::remove_dir_all(&dir);
    // Intentionally do NOT create the file.
    let path = dir.join("config.json");

    let prev = std::env::var_os("COS_CONFIG_PATH");
    std::env::set_var("COS_CONFIG_PATH", &path);
    let cfg = load_from_disk();
    match prev {
        Some(x) => std::env::set_var("COS_CONFIG_PATH", x),
        None => std::env::remove_var("COS_CONFIG_PATH"),
    }

    assert_eq!(cfg.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(cfg.exec.timeout, 300);
}

#[test]
fn load_from_path_round_trip() {
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("cos-config-roundtrip-{pid}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.json");
    fs::write(
        &path,
        r#"{"agent": {"provider": "copilot", "model": "claude-opus-4.7"}}"#,
    )
    .unwrap();

    let cfg = load_from_path(&path);
    assert_eq!(cfg.agent.provider, "copilot");
    assert_eq!(cfg.agent.model, "claude-opus-4.7");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_from_path_missing_file_returns_defaults() {
    let cfg = load_from_path(Path::new("/nonexistent/cos-config-xyzzy.json"));
    // No panic, no error — just defaults.
    assert_eq!(cfg.agent.provider, "");
}

#[tokio::test]
async fn with_override_swaps_get_inside_scope_only() {
    // Establish a baseline: outside any scope, no override.
    assert!(current_override().is_none());

    let mut cfg = CosConfig::default();
    cfg.agent.provider = "copilot".into();
    cfg.agent.model = "claude-opus-4.7".into();
    let leaked = intern_static(cfg);

    with_override(leaked, async {
        let inside = get();
        assert_eq!(inside.agent.provider, "copilot");
        assert_eq!(inside.agent.model, "claude-opus-4.7");
        assert!(current_override().is_some());
    })
    .await;

    // After the scope ends, the override is gone again.
    assert!(current_override().is_none());
}

#[tokio::test]
async fn override_propagates_through_awaited_futures() {
    // tokio::task_local values flow through `.await` and through
    // primitives like `futures::join_all` that drive their inner
    // futures from the same task. They do NOT auto-propagate
    // across `tokio::spawn` — see
    // `override_does_not_leak_across_spawn` for that boundary.
    // The agent dispatch loop (turn.rs) joins concurrent tool
    // futures with `join_all`, so the user's config is visible
    // inside every tool's `exec` regardless of how many run in
    // parallel.
    let mut cfg = CosConfig::default();
    cfg.agent.provider = "openai".into();
    let leaked = intern_static(cfg);

    let observed = with_override(leaked, async move {
        // Plain `.await` of a child future.
        let a = async { get().agent.provider.clone() }.await;
        // join_all-style concurrency drives futures within the
        // same task, so the override is still visible.
        let bcd =
            futures_util::future::join_all((0..3).map(|_| async { get().agent.provider.clone() }))
                .await;
        (a, bcd)
    })
    .await;

    assert_eq!(observed.0, "openai");
    assert_eq!(observed.1, vec!["openai", "openai", "openai"]);
}

#[tokio::test]
async fn override_does_not_leak_across_spawn() {
    // tokio::spawn creates a fresh task that does NOT inherit
    // parent task_local values. This is the documented tokio
    // contract; we keep it explicit because any future code that
    // spawns background work and reads `config::get()` must
    // re-scope the override itself (or capture an `&CosConfig`
    // and pass it through).
    let mut cfg = CosConfig::default();
    cfg.agent.provider = "openai".into();
    let leaked = intern_static(cfg);

    let observed = with_override(leaked, async move {
        tokio::spawn(async move {
            // Inside the spawned task: no override, so this
            // returns the process-wide config (defaults in the
            // unit-test environment) and definitely NOT
            // "openai".
            get().agent.provider.clone()
        })
        .await
        .unwrap()
    })
    .await;

    assert_ne!(observed, "openai");
}

#[test]
fn intern_static_dedupes_by_content() {
    let mut a = CosConfig::default();
    a.agent.provider = "anthropic".into();
    a.agent.model = "claude-sonnet-4.6".into();

    let mut b = CosConfig::default();
    b.agent.provider = "anthropic".into();
    b.agent.model = "claude-sonnet-4.6".into();

    let p1 = intern_static(a) as *const CosConfig;
    let p2 = intern_static(b) as *const CosConfig;
    assert_eq!(p1, p2, "identical config payloads must share leaked slot");

    let mut c = CosConfig::default();
    c.agent.provider = "anthropic".into();
    c.agent.model = "claude-opus-4.7".into();
    let p3 = intern_static(c) as *const CosConfig;
    assert_ne!(p1, p3, "distinct config payloads must not dedupe");
}

#[test]
fn intern_for_home_reads_users_config_json() {
    let pid = std::process::id();
    let home = std::env::temp_dir().join(format!("cos-config-home-{pid}"));
    let _ = fs::remove_dir_all(&home);
    let cfg_path = crate::paths::user_config_path_for(&home);
    fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
    fs::write(
        &cfg_path,
        r#"{"agent": {"provider": "xai", "model": "grok-2"}}"#,
    )
    .unwrap();

    let interned = intern_for_home(&home);
    assert_eq!(interned.agent.provider, "xai");
    assert_eq!(interned.agent.model, "grok-2");

    let _ = fs::remove_dir_all(&home);
}
