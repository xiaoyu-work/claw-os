//! Encrypted credential storage, authorization, lifecycle, OAuth, and CLI facade.
//!
//! The public functions in this module are compatibility entry points. Secret
//! material remains behind the store API; command and OAuth code coordinate
//! typed store operations without accessing key or file internals.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

use crate::caps::{require_or_json, Scope, Verb};
use crate::policy;

mod authorization;
mod cli;
mod crypto;
mod domain;
mod error;
mod keyring;
mod lifecycle;
mod master_key;
mod oauth;
mod oauth_login;
mod store;

use authorization::{
    bundle_scope, credential_scope, effective_session_tier, is_expired, namespace_scope,
    require_credential_access, require_secret, tier_grants_access, validate_credential_component,
};
use cli::parse_namespace_flag;
use crypto::{decrypt_value, encrypt_value, sha256};
use domain::{
    BundleManifest, CredentialId, CredentialMetadata, CredentialStore, LoadedBundle,
    LoadedCredential, NamespaceId, NamespaceSummary, StoreRequest, StoreResult, StoredCredential,
};
pub use error::CredentialResult;
pub use error::{CredentialError, CredentialErrorKind};
#[cfg(all(test, target_os = "linux"))]
use keyring::inject_keyring_failure;
use keyring::{cache_master_key, read_master_key};
use lifecycle::{load_bundle, load_credential};
use master_key::{derive_key, generate_nonce, legacy_xor};
use oauth::{
    broker_oauth_provider, cmd_oauth_refresh, direct_oauth_refresh, http_post,
    request_brokered_oauth_refresh, urlencoded,
};
use store::{list_all_namespaces, list_namespace, revoke, FileCredentialStore, FILE_STORE};
#[cfg(test)]
use {
    cli::{cmd_bundle, cmd_list, cmd_load, cmd_load_bundle, cmd_revoke, cmd_store},
    crypto::{aes_gcm, from_b64, to_b64},
    keyring::MASTER_KEY_LABEL,
    lifecycle::{compute_original_ttl, execute_refresh},
    master_key::{
        generate_and_persist_root_key_at, generate_root_key_at_barrier,
        inject_root_key_random_failure, inject_root_key_write_failure, legacy_obfuscation_key,
        load_persistent_root_key_at,
    },
    oauth::build_curl_post,
    store::{namespace_dir, refresh_sentinel_path, with_refresh_lock, write_credential_atomic},
};

pub(crate) use cli::run_agent_oauth_login;
pub use cli::{run, run_typed};
pub(crate) use master_key::os_random_bytes;
pub(crate) use store::load_for_broker;
pub use store::{
    is_configured, is_configured_typed, load_for_scheduler, load_for_scheduler_typed,
    load_optional_for_scheduler, load_optional_for_scheduler_typed, rollback_delete,
    rollback_delete_typed, rollback_restore, rollback_restore_typed, try_load, try_load_typed,
};

pub(crate) fn broker_refresh_access_token(name: &str, namespace: &str) -> Result<Value, String> {
    oauth::broker_refresh_access_token(&FILE_STORE, name, namespace)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/credential/mod.rs"
    ));
}
