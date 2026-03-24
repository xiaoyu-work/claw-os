/// Configuration loading for Claw OS.
///
/// Reads `/etc/cos/config.json` (or `COS_CONFIG_PATH` override) and
/// provides typed access to settings. Falls back to sensible defaults
/// if the config file is missing or malformed.
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

static CONFIG: OnceLock<CosConfig> = OnceLock::new();

const DEFAULT_CONFIG_PATH: &str = "/etc/cos/config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosConfig {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_den")]
    pub den: String,
    #[serde(default)]
    pub exec: ExecConfig,
    #[serde(default)]
    pub net: NetConfig,
    #[serde(default)]
    pub web: WebConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecConfig {
    #[serde(default = "default_exec_timeout")]
    pub timeout: u64,
    #[serde(default = "default_shell")]
    pub shell: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetConfig {
    #[serde(default = "default_net_timeout")]
    pub timeout: u64,
    #[serde(default = "default_true")]
    pub allow_outbound: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    #[serde(default = "default_reader_url")]
    pub reader_url: String,
    #[serde(default = "default_net_timeout")]
    pub timeout: u64,
    #[serde(default = "default_max_content_length")]
    pub max_content_length: usize,
}

fn default_version() -> String { "0.3.0".into() }
fn default_den() -> String { "/den".into() }
fn default_exec_timeout() -> u64 { 300 }
fn default_shell() -> String { "/bin/bash".into() }
fn default_net_timeout() -> u64 { 30 }
fn default_true() -> bool { true }
fn default_reader_url() -> String { "http://localhost:3000".into() }
fn default_max_content_length() -> usize { 50000 }

impl Default for ExecConfig {
    fn default() -> Self {
        Self { timeout: default_exec_timeout(), shell: default_shell() }
    }
}

impl Default for NetConfig {
    fn default() -> Self {
        Self { timeout: default_net_timeout(), allow_outbound: true }
    }
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            reader_url: default_reader_url(),
            timeout: default_net_timeout(),
            max_content_length: default_max_content_length(),
        }
    }
}

impl Default for CosConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            den: default_den(),
            exec: ExecConfig::default(),
            net: NetConfig::default(),
            web: WebConfig::default(),
        }
    }
}

/// Load config from disk, or return defaults if file is missing/invalid.
fn load_from_disk() -> CosConfig {
    let path = std::env::var("COS_CONFIG_PATH")
        .unwrap_or_else(|_| DEFAULT_CONFIG_PATH.into());

    let path = Path::new(&path);
    if !path.is_file() {
        return CosConfig::default();
    }

    match fs::read_to_string(path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => CosConfig::default(),
    }
}

/// Get the global config (loaded once, cached).
pub fn get() -> &'static CosConfig {
    CONFIG.get_or_init(load_from_disk)
}

/// Return config values as environment variables for Python app subprocesses.
pub fn as_env_vars() -> Vec<(String, String)> {
    let cfg = get();
    vec![
        ("COS_EXEC_TIMEOUT".into(), cfg.exec.timeout.to_string()),
        ("COS_EXEC_SHELL".into(), cfg.exec.shell.clone()),
        ("COS_NET_TIMEOUT".into(), cfg.net.timeout.to_string()),
        ("COS_NET_ALLOW_OUTBOUND".into(), cfg.net.allow_outbound.to_string()),
        ("COS_WEB_READER_URL".into(), cfg.web.reader_url.clone()),
        ("COS_WEB_TIMEOUT".into(), cfg.web.timeout.to_string()),
        ("COS_WEB_MAX_CONTENT_LENGTH".into(), cfg.web.max_content_length.to_string()),
        ("COS_DEN".into(), cfg.den.clone()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_values() {
        let cfg = CosConfig::default();
        assert_eq!(cfg.version, "0.3.0");
        assert_eq!(cfg.den, "/den");
        assert_eq!(cfg.exec.timeout, 300);
        assert_eq!(cfg.exec.shell, "/bin/bash");
        assert_eq!(cfg.net.timeout, 30);
        assert!(cfg.net.allow_outbound);
        assert_eq!(cfg.web.reader_url, "http://localhost:3000");
        assert_eq!(cfg.web.max_content_length, 50000);
    }

    #[test]
    fn parse_partial_config() {
        let json = r#"{"version": "1.0.0", "den": "/custom"}"#;
        let cfg: CosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.version, "1.0.0");
        assert_eq!(cfg.den, "/custom");
        // Defaults for missing sections
        assert_eq!(cfg.exec.timeout, 300);
        assert_eq!(cfg.web.reader_url, "http://localhost:3000");
    }

    #[test]
    fn parse_full_config() {
        let json = r#"{
            "version": "0.3.0",
            "den": "/den",
            "exec": {"timeout": 600, "shell": "/bin/zsh"},
            "net": {"timeout": 10, "allow_outbound": false},
            "web": {"reader_url": "http://custom:5000", "timeout": 60, "max_content_length": 100000}
        }"#;
        let cfg: CosConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.exec.timeout, 600);
        assert_eq!(cfg.exec.shell, "/bin/zsh");
        assert_eq!(cfg.net.timeout, 10);
        assert!(!cfg.net.allow_outbound);
        assert_eq!(cfg.web.reader_url, "http://custom:5000");
        assert_eq!(cfg.web.max_content_length, 100000);
    }

    #[test]
    fn as_env_vars_returns_all_keys() {
        let vars = as_env_vars();
        let keys: Vec<&str> = vars.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"COS_EXEC_TIMEOUT"));
        assert!(keys.contains(&"COS_NET_TIMEOUT"));
        assert!(keys.contains(&"COS_WEB_READER_URL"));
        assert!(keys.contains(&"COS_DEN"));
    }

    #[test]
    fn malformed_json_returns_defaults() {
        let json = "not valid json {{{";
        let cfg: CosConfig = serde_json::from_str(json).unwrap_or_default();
        assert_eq!(cfg.version, "0.3.0");
        assert_eq!(cfg.exec.timeout, 300);
    }
}
