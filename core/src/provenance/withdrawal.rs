//! Read-only, denial-only owner policy. Never an additional publisher authority.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use super::envelope::{is_sha256_ref, PackageKind};
use super::runtime::PackageRef;
use super::state::{TrustDomain, TrustWatch};
use super::trust::{TrustRootSpec, TrustStore};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Target {
    Revocation {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        digest: Option<String>,
    },
    Developer {
        kind: PackageKind,
        id: String,
    },
}

impl Target {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Revocation { key_id, digest } => {
                if key_id.is_none() && digest.is_none() {
                    return Err("withdrawal requires a key id and/or content digest".into());
                }
                if key_id
                    .iter()
                    .chain(digest)
                    .any(|value| !is_sha256_ref(value))
                {
                    return Err(
                        "withdrawal references must be canonical sha256:<64 lowercase hex>".into(),
                    );
                }
            }
            Self::Developer { id, .. } => {
                if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
                    return Err("developer withdrawal requires a bounded package id".into());
                }
            }
        }
        Ok(())
    }

    pub fn matches(&self, package: &PackageRef) -> bool {
        match self {
            Self::Revocation { key_id, digest } => {
                key_id
                    .as_ref()
                    .is_some_and(|key| package.publisher_key_id.as_ref() == Some(key))
                    || digest.as_ref() == Some(&package.content_digest)
            }
            Self::Developer { kind, id } => {
                package.kind == *kind && package.id == *id && package.tier == "developer"
            }
        }
    }

    pub fn observed(&self, policy: &OwnerPolicy) -> bool {
        match self {
            Self::Revocation { key_id, digest } => {
                key_id
                    .as_ref()
                    .is_none_or(|key| policy.store.is_key_revoked(key))
                    && digest
                        .as_ref()
                        .is_none_or(|digest| policy.store.is_package_revoked(digest))
            }
            // The retained binding's exact path/digest decides whether removal
            // changed authority; another grant for this id is not equivalent.
            Self::Developer { .. } => true,
        }
    }
}

#[derive(Clone)]
pub struct OwnerPolicy {
    pub owner: u32,
    pub generation: Option<u64>,
    pub fingerprint: String,
    pub store: Arc<TrustStore>,
}

impl OwnerPolicy {
    pub fn load(owner: u32) -> Result<Self, String> {
        type Cache = BTreeMap<u32, (Vec<TrustRootSpec>, OwnerPolicy)>;
        static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
        let home = crate::paths::verified_home_for_uid(owner)?;
        let roots: Vec<_> = TrustStore::roots_for_owner(owner)
            .into_iter()
            .filter(|root| root.domain == TrustDomain::Owner(owner))
            .collect();
        if roots.len() != 2 || roots.iter().any(|root| !root.path.starts_with(&home)) {
            return Err("owner withdrawal roots could not be authenticated".into());
        }
        let cache = CACHE.get_or_init(Default::default);
        {
            let cache = cache
                .lock()
                .map_err(|_| "owner withdrawal cache is unavailable")?;
            if let Some((previous_roots, policy)) = cache.get(&owner) {
                if *previous_roots == roots && policy.store.is_current(&roots) {
                    return Ok(policy.clone());
                }
            }
        }
        let policy = Self::load_roots(owner, &roots)?;
        let mut cache = cache
            .lock()
            .map_err(|_| "owner withdrawal cache is unavailable")?;
        if cache.len() >= 128 && !cache.contains_key(&owner) {
            if let Some(key) = cache.keys().copied().find(|key| *key != 0) {
                cache.remove(&key);
            }
        }
        cache.insert(owner, (roots, policy.clone()));
        Ok(policy)
    }

    pub(crate) fn load_roots(owner: u32, roots: &[TrustRootSpec]) -> Result<Self, String> {
        for _ in 0..3 {
            let before = TrustWatch::observe(&TrustStore::watch_paths(roots));
            let store = TrustStore::load_roots(roots);
            let after = TrustWatch::observe(&TrustStore::watch_paths(roots));
            if before != after {
                continue;
            }
            let domain = TrustDomain::Owner(owner);
            if !store.domain_is_usable(domain) || !store.diagnostics().is_empty() {
                return Err("owner withdrawal policy is unreadable or has an unrecorded generation; authority remains denied".into());
            }
            let state_dir = roots
                .first()
                .ok_or("owner policy has no roots")?
                .state_dir();
            let recorded =
                super::state::read_state(&state_dir, domain).map_err(|error| error.to_string())?;
            if recorded.as_ref().map(|state| state.generation) != store.domain_generation(domain)
                || !store.is_current(roots)
            {
                continue;
            }
            return Ok(Self {
                owner,
                generation: recorded.as_ref().map(|state| state.generation),
                fingerprint: recorded
                    .map(|state| state.fingerprint)
                    .unwrap_or_else(|| super::state::fingerprint_files(&[])),
                store: Arc::new(store),
            });
        }
        Err(
            "owner withdrawal policy changed during inspection; retry without admitting authority"
                .into(),
        )
    }

    pub fn check_denials(&self, package: &PackageRef) -> Result<(), String> {
        if self.store.is_package_revoked(&package.content_digest) {
            return Err("consumer policy withdrew this exact package digest".into());
        }
        if package
            .publisher_key_id
            .as_ref()
            .is_some_and(|key| self.store.is_key_revoked(key))
        {
            return Err("consumer policy withdrew this publisher key".into());
        }
        // Missing keys or developer grants here never change the positive
        // verifier's supporting context, and can never add authority.
        Ok(())
    }
}
