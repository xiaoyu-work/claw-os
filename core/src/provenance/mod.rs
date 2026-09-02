//! Extension package provenance: one authentication gate for Apps,
//! Skills and MCP/adapter packages.
//!
//! Nothing an extension ships — manifest, operations, capability
//! `needs`, tool schemas, skill instructions, model-visible metadata —
//! may be trusted until [`verify::verify_package`] has authenticated
//! the publisher and the complete file tree.
//!
//! ## Layout
//!
//! | Module | Responsibility |
//! | --- | --- |
//! | [`envelope`] | The versioned `claw.provenance/v1` format and its canonical signing bytes |
//! | [`trust`] | Trust roots, key ids, revocation/rotation, developer grants |
//! | [`verify`] | Signature + content verification, verified snapshots |
//! | [`sign`] | Publisher/developer signing workflow |
//! | [`install`] | Bounded staging, atomic content-addressed publication |
//! | [`fsec`] | Ownership/mode gating and TOCTOU-resistant reads |
//! | [`cli`] | `cos provenance …` |
//!
//! ## Secure by default
//!
//! User-installed extensions require a valid signature from a trusted,
//! non-revoked key. There is no environment variable that turns
//! verification off or adds a trust root; unsigned local development
//! goes through `cos provenance dev-trust`, which records a persistent
//! decision in a segregated developer root and caps what the package
//! may do.

pub mod ceiling;
pub mod cli;
pub mod consent;
pub mod envelope;
pub mod fsec;
pub mod install;
pub mod runtime;
pub mod sign;
pub mod state;
pub mod trust;
pub mod verify;

use std::sync::{Arc, Mutex, OnceLock};

pub use ceiling::Ceiling;
pub use envelope::{Envelope, PackageKind};
pub use trust::{TrustStore, TrustTier};
pub use verify::{ProvenanceError, TrustSource, VerifiedPackage, VerifyOptions};

type TrustCache = (Arc<TrustStore>, Vec<trust::TrustRootSpec>);

fn trust_cell() -> &'static Mutex<Option<TrustCache>> {
    static STORE: OnceLock<Mutex<Option<TrustCache>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(None))
}

/// Process-wide trust store, kept current for long-lived daemons.
///
/// `clawd` and `claw-agentd` run for days. A store cached once at
/// startup would keep honouring a key the operator revoked minutes
/// ago, so every call re-checks the durable per-domain state before
/// handing the store back. The check is a handful of `lstat` calls
/// against the state files and root directories; only an actual
/// change pays for a reload.
///
/// Call this on the authority path — before a launch, a disclosure, an
/// attach or a relay — not once at startup.
pub fn trust_store() -> Arc<TrustStore> {
    let mut guard = trust_cell().lock().unwrap_or_else(|p| p.into_inner());
    if let Some((existing, roots)) = guard.as_ref() {
        if existing.is_current(roots) {
            return Arc::clone(existing);
        }
        // Something under a trust root moved. Drop every cached
        // verification before the new store is published so no consumer
        // can observe a snapshot verified under the previous
        // generation.
        verify::invalidate_cache();
        tracing::info!(
            target: "provenance",
            "trust roots changed; reloading publisher trust and invalidating verification caches"
        );
    }
    let roots = guard
        .as_ref()
        .map(|(_, roots)| roots.clone())
        .unwrap_or_else(TrustStore::default_roots);
    let store = Arc::new(TrustStore::load_roots(&roots));
    *guard = Some((Arc::clone(&store), roots));
    store
}

/// Install an explicit trust store for the current process. Used by
/// the CLI when an operator points at a specific root and by tests.
/// There is no environment variable that reaches this.
pub fn set_trust_store_for_roots(
    store: TrustStore,
    roots: Vec<trust::TrustRootSpec>,
) -> Arc<TrustStore> {
    let store = Arc::new(store);
    verify::invalidate_cache();
    let mut guard = trust_cell().lock().unwrap_or_else(|p| p.into_inner());
    *guard = Some((Arc::clone(&store), roots));
    store
}

/// Install a store built from the production roots.
pub fn set_trust_store(store: TrustStore) -> Arc<TrustStore> {
    set_trust_store_for_roots(store, TrustStore::default_roots())
}

/// Re-read the trust roots and invalidate every cached verification.
pub fn reload_trust() -> Arc<TrustStore> {
    verify::invalidate_cache();
    let roots = {
        let mut guard = trust_cell().lock().unwrap_or_else(|p| p.into_inner());
        let roots = guard
            .as_ref()
            .map(|(_, roots)| roots.clone())
            .unwrap_or_else(TrustStore::default_roots);
        *guard = None;
        roots
    };
    let store = Arc::new(TrustStore::load_roots(&roots));
    let mut guard = trust_cell().lock().unwrap_or_else(|p| p.into_inner());
    *guard = Some((Arc::clone(&store), roots));
    store
}

/// Emit a structured provenance audit record.
///
/// Records reference the publisher key id and the package content
/// digest. They never contain key material, bundle bytes or
/// model-visible text.
pub fn audit(event: &str, facts: serde_json::Value) {
    let mut entry = facts;
    if let Some(map) = entry.as_object_mut() {
        map.insert(
            "kind".to_string(),
            serde_json::Value::String(event.to_string()),
        );
    }
    crate::audit::log_event(&crate::paths::provenance_audit_path(), entry);
}

/// Human-readable, actionable diagnostic for a rejected package.
/// Discovery uses this instead of skipping silently.
pub fn quarantine_reason(kind: PackageKind, id: &str, error: &ProvenanceError) -> String {
    format!(
        "{} `{id}` is quarantined: {error}. Re-install a signed package, \
         or run `cos provenance dev-trust --kind {} --id {id} --path <dir>` \
         to accept an unsigned development tree with a restricted ceiling.",
        kind.as_str(),
        kind.as_str()
    )
}
