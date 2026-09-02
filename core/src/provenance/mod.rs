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

fn owner_trust_cells() -> &'static Mutex<std::collections::HashMap<u32, TrustCache>> {
    static STORES: OnceLock<Mutex<std::collections::HashMap<u32, TrustCache>>> = OnceLock::new();
    STORES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

tokio::task_local! {
    static TRUST_OWNER_OVERRIDE: u32;
}

pub(crate) async fn with_trust_owner<F, R>(owner_uid: u32, future: F) -> R
where
    F: std::future::Future<Output = R>,
{
    TRUST_OWNER_OVERRIDE.scope(owner_uid, future).await
}

pub(crate) fn current_trust_owner() -> Option<u32> {
    TRUST_OWNER_OVERRIDE.try_with(|owner| *owner).ok()
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
    if let Ok(owner_uid) = TRUST_OWNER_OVERRIDE.try_with(|owner| *owner) {
        if owner_uid != fsec::effective_uid() {
            return trust_store_for_owner(owner_uid);
        }
    }
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

/// Trust store for an owner identity authenticated by the daemon or Host.
pub fn trust_store_for_owner(owner_uid: u32) -> Arc<TrustStore> {
    if owner_uid == fsec::effective_uid() {
        return trust_store();
    }
    {
        let guard = trust_cell().lock().unwrap_or_else(|p| p.into_inner());
        if let Some((store, roots)) = guard.as_ref() {
            if *roots != TrustStore::default_roots() {
                publish_routed_trust_if_present(owner_uid, store);
                return Arc::clone(store);
            }
        }
    }
    #[cfg(target_os = "linux")]
    if fsec::effective_uid() != 0 {
        return crate::storage::read_routed_trust_snapshot(owner_uid)
            .and_then(|bytes| TrustStore::from_routed_snapshot(owner_uid, &bytes))
            .map(Arc::new)
            .unwrap_or_else(|error| {
                tracing::error!(
                    target: "provenance",
                    owner_uid,
                    %error,
                    "owner trust snapshot is unavailable; failing the trust domain closed"
                );
                Arc::new(TrustStore::default())
            });
    }
    let roots = TrustStore::roots_for_owner(owner_uid);
    let mut stores = owner_trust_cells()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some((store, cached_roots)) = stores.get(&owner_uid) {
        if *cached_roots == roots && store.is_current(cached_roots) {
            publish_routed_trust_if_present(owner_uid, store);
            return Arc::clone(store);
        }
        verify::invalidate_cache();
    }
    let store = Arc::new(TrustStore::load_roots(&roots));
    stores.insert(owner_uid, (Arc::clone(&store), roots));
    publish_routed_trust_if_present(owner_uid, &store);
    store
}

fn publish_routed_trust_if_present(owner_uid: u32, store: &TrustStore) {
    #[cfg(target_os = "linux")]
    {
        if fsec::effective_uid() != 0
            || !std::path::Path::new("/run/cos/caps")
                .join(owner_uid.to_string())
                .is_dir()
        {
            return;
        }
        if let Err(error) = store
            .routed_snapshot_bytes(owner_uid)
            .and_then(|bytes| crate::storage::write_routed_trust_snapshot(owner_uid, &bytes))
        {
            tracing::error!(
                target: "provenance",
                owner_uid,
                %error,
                "could not publish routed owner trust snapshot"
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (owner_uid, store);
}

pub(crate) fn refresh_owner_trust_snapshot(owner_uid: u32) -> Result<(), String> {
    if fsec::effective_uid() != 0 {
        return Err("owner trust snapshots require a root broker".to_string());
    }
    let explicit = {
        let guard = trust_cell().lock().unwrap_or_else(|p| p.into_inner());
        guard.as_ref().and_then(|(store, roots)| {
            (*roots != TrustStore::default_roots()).then(|| (Arc::clone(store), roots.clone()))
        })
    };
    let (store, roots) = match explicit {
        Some((store, roots)) => (store, roots),
        None => {
            let roots = TrustStore::roots_for_owner(owner_uid);
            (Arc::new(TrustStore::load_roots(&roots)), roots)
        }
    };
    let bytes = store.routed_snapshot_bytes(owner_uid)?;
    crate::storage::write_routed_trust_snapshot(owner_uid, &bytes)?;
    let mut stores = owner_trust_cells()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    stores.insert(owner_uid, (store, roots));
    Ok(())
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
    owner_trust_cells()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
    store
}

/// Install a store built from the production roots.
pub fn set_trust_store(store: TrustStore) -> Arc<TrustStore> {
    set_trust_store_for_roots(store, TrustStore::default_roots())
}

/// Re-read the trust roots and invalidate every cached verification.
pub fn reload_trust() -> Arc<TrustStore> {
    verify::invalidate_cache();
    owner_trust_cells()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
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
