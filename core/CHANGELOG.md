# Core changelog

## 0.2.0

### Runtime dependency API

- Added `config::current_snapshot() -> Arc<CosConfig>` and
  `config::with_snapshot(Arc<CosConfig>, future)` for request-scoped runtime
  composition.
- Preserved `config::get() -> &'static CosConfig` and
  `config::with_override(&'static CosConfig, future)` for existing static
  callers.
- Removed `intern_for_home` and `intern_user_config`. Their old
  `&'static CosConfig` contract required retaining or leaking every dynamic
  configuration. Migrate to `load_for_home` and `load_user_config`, which
  return owned `Arc<CosConfig>` snapshots, then use `with_snapshot`.
- Added `default_registry_with_deps` and
  `register_default_media_tools_with_outputs_dir`. The old no-argument
  `default_registry` and one-argument `register_default_media_tools` remain as
  deprecated composition wrappers.
- Detached runtime work captures and reinstalls `RoutedPathContext`, preserving
  owner-partitioned budget, audit, notes, credential, and user-state paths
  across Tokio task boundaries.
