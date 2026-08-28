use super::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn current_override() -> Option<Arc<CosConfig>> {
    CONFIG_SNAPSHOT_OVERRIDE.try_with(Arc::clone).ok()
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
    assert_eq!(cfg.agent.max_turns, 50);
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
async fn with_snapshot_swaps_current_snapshot_inside_scope_only() {
    // Establish a baseline: outside any scope, no override.
    assert!(current_override().is_none());

    let mut cfg = CosConfig::default();
    cfg.agent.provider = "copilot".into();
    cfg.agent.model = "claude-opus-4.7".into();
    let scoped = Arc::new(cfg);

    with_snapshot(scoped, async {
        let inside = current_snapshot();
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
    let scoped = Arc::new(cfg);

    let observed = with_snapshot(scoped, async move {
        // Plain `.await` of a child future.
        let a = async { current_snapshot().agent.provider.clone() }.await;
        // join_all-style concurrency drives futures within the
        // same task, so the override is still visible.
        let bcd =
            futures_util::future::join_all(
                (0..3).map(|_| async { current_snapshot().agent.provider.clone() }),
            )
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
    let scoped = Arc::new(cfg);

    let observed = with_snapshot(scoped, async move {
        tokio::spawn(async move {
            // Inside the spawned task: no override, so this
            // returns the process-wide config (defaults in the
            // unit-test environment) and definitely NOT
            // "openai".
            current_snapshot().agent.provider.clone()
        })
        .await
        .unwrap()
    })
    .await;

    assert_ne!(observed, "openai");
}

#[tokio::test]
async fn parallel_override_scopes_are_isolated_and_reclaimed() {
    let mut left = CosConfig::default();
    left.agent.provider = "left".into();
    let left = Arc::new(left);
    let left_weak = Arc::downgrade(&left);

    let mut right = CosConfig::default();
    right.agent.provider = "right".into();
    let right = Arc::new(right);
    let right_weak = Arc::downgrade(&right);

    let (seen_left, seen_right) = tokio::join!(
        with_snapshot(left, async {
            tokio::task::yield_now().await;
            current_snapshot().agent.provider.clone()
        }),
        with_snapshot(right, async {
            tokio::task::yield_now().await;
            current_snapshot().agent.provider.clone()
        })
    );

    assert_eq!(seen_left, "left");
    assert_eq!(seen_right, "right");
    assert!(left_weak.upgrade().is_none());
    assert!(right_weak.upgrade().is_none());
    assert!(current_override().is_none());
}

#[test]
fn scoped_config_is_reclaimed_after_scope() {
    let scoped = Arc::new(CosConfig::default());
    let weak = Arc::downgrade(&scoped);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(with_snapshot(scoped, async {}));
    assert!(weak.upgrade().is_none());
}

#[test]
#[allow(deprecated)]
fn legacy_static_config_api_remains_source_compatible() {
    fn accepts_config(_: &CosConfig) {}

    accepts_config(get());
    let _: CosConfig = get().clone();

    static LEGACY: OnceLock<CosConfig> = OnceLock::new();
    let legacy = LEGACY.get_or_init(|| {
        let mut config = CosConfig::default();
        config.agent.provider = "legacy".into();
        config
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let observed = runtime.block_on(with_override(legacy, async {
        get().agent.provider.clone()
    }));
    assert_eq!(observed, "legacy");
}

#[test]
fn production_code_uses_owned_config_snapshots_not_legacy_get() {
    fn visit(path: &Path, offenders: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, offenders);
            } else if path.extension().and_then(|value| value.to_str()) == Some("rs")
                && path.file_name().and_then(|value| value.to_str()) != Some("config.rs")
            {
                let source = fs::read_to_string(&path).unwrap();
                if source.contains("config::get()") || source.contains("crate::config::get()") {
                    offenders.push(path);
                }
            }
        }
    }

    let mut offenders = Vec::new();
    visit(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut offenders,
    );
    assert!(
        offenders.is_empty(),
        "production code must use config::current_snapshot(): {offenders:?}"
    );
}

#[test]
fn load_for_home_reads_users_config_json() {
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

    let loaded = load_for_home(&home);
    assert_eq!(loaded.agent.provider, "xai");
    assert_eq!(loaded.agent.model, "grok-2");

    let _ = fs::remove_dir_all(&home);
}
