use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub(super) const APP_ID: &str = "host-process";
pub(super) const OPERATION: &str = "read";
pub(super) const BODY: &str = "controlled App process body\n";
pub(super) const CONTEXT_ENV: &str = "COS_APP_HOST_PROCESS_CONTEXT";
pub(super) const CHILD_TEST: &str =
    "agentd::worker::tests::app_host_process::controlled_host_child";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProcessContext {
    pub root: PathBuf,
    pub uid: u32,
    pub gid: u32,
    pub mount_namespace: String,
}

impl ProcessContext {
    pub fn input(&self) -> PathBuf {
        self.root.join("input.txt")
    }

    pub fn apps(&self) -> PathBuf {
        self.root.join("apps")
    }

    pub fn app_data(&self) -> PathBuf {
        self.home()
            .join(".local")
            .join("share")
            .join("cos")
            .join("apps")
            .join(APP_ID)
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    pub fn trust_roots(&self) -> Vec<crate::provenance::trust::TrustRootSpec> {
        vec![crate::provenance::trust::TrustRootSpec {
            path: self.root.join("trust").join("publishers.d"),
            tier: crate::provenance::TrustTier::System,
            allowed_uids: vec![0],
            domain: crate::provenance::state::TrustDomain::System,
        }]
    }

    pub fn install_test_trust(&self) {
        let roots = self.trust_roots();
        let store = crate::provenance::TrustStore::load_roots(&roots);
        assert!(
            !store.is_empty(),
            "test publisher: {:?}",
            store.diagnostics()
        );
        crate::provenance::set_trust_store_for_roots(store, roots);
    }
}
