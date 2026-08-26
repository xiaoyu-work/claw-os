use super::*;
use crate::agent::memory::sqlite_fts::MemoryDb;

fn mem_db() -> MemoryDb {
    MemoryDb::open_in_memory().expect("in-memory db")
}

/// Curator stays off when the main provider is unconfigured (empty
/// — the default for fresh installs) or `mock`. Avoids spending
/// cycles on canned/non-existent responses.
#[test]
fn auto_curator_disabled_when_main_is_unconfigured() {
    let cfg = AgentConfig::default();
    assert!(cfg.provider.is_empty());
    assert!(AutoCurator::from_cfg_logged(&cfg, &mem_db()).is_none());
}

#[test]
fn auto_curator_disabled_when_main_is_mock() {
    let mut cfg = AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    assert!(AutoCurator::from_cfg_logged(&cfg, &mem_db()).is_none());
}

/// When `auxiliary_provider` is unset but the main provider is a
/// real LLM, the curator falls back to it — that's the
/// default-on path the user asked for.
#[test]
fn auto_curator_falls_back_to_main_when_aux_unset() {
    let mut cfg = AgentConfig::default();
    cfg.provider = "openai".into();
    cfg.model = "gpt-4o-mini".into();
    cfg.api_key_env = Some("OPENAI_API_KEY".into());
    assert!(cfg.auxiliary_provider.is_none());
    assert!(AutoCurator::from_cfg_logged(&cfg, &mem_db()).is_some());
}

/// Explicit `auxiliary_provider` still wins (the fallback is a
/// pure default-on path; it does not override an explicit
/// setting).
#[test]
fn auto_curator_respects_explicit_aux() {
    let mut cfg = AgentConfig::default();
    cfg.provider = "openai".into();
    cfg.model = "gpt-4o".into();
    cfg.auxiliary_provider = Some("openai".into());
    cfg.auxiliary_model = Some("gpt-4o-mini".into());
    cfg.api_key_env = Some("OPENAI_API_KEY".into());
    assert!(AutoCurator::from_cfg_logged(&cfg, &mem_db()).is_some());
}

/// `aux_from_main` errors when the main model is empty — we
/// shouldn't silently swallow a malformed config.
#[test]
fn aux_from_main_errors_when_model_empty() {
    let mut cfg = AgentConfig::default();
    cfg.provider = "openai".into();
    cfg.model = String::new();
    assert!(aux_from_main(&cfg).is_err());
}
