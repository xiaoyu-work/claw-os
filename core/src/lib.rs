// Several staged runtime surfaces are compiled before their CLI wiring lands,
// and long protocol/FFI signatures intentionally mirror external schemas.
#![allow(dead_code)]
#![allow(
    clippy::doc_lazy_continuation,
    clippy::doc_overindented_list_items,
    clippy::should_implement_trait,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

pub mod agent;
pub mod agentd;
pub mod ai;
pub mod approvals;
pub mod apps;
pub mod audit;
pub mod audit_policy;
pub mod bridge;
pub mod browser;
pub mod caps;
pub mod checkpoint;
pub(crate) mod cli_catalog;
pub(crate) mod cli_help;
pub mod clawd;
pub mod config;
pub mod credential;
pub mod cron;
pub mod crypto;
pub mod engine_pkg;
pub mod errors;
pub mod filelock;
pub mod i18n;
pub mod ipc;
pub mod model;
pub mod netfilter;
pub mod notifications;
pub mod paths;
pub mod perms;
pub mod policy;
pub mod proc;
pub mod provenance;
pub mod router;
pub mod mem_bridge;
pub mod sandbox;
pub mod service;
pub mod session;
pub mod storage;
pub mod sysinfo;
pub mod trace;
pub mod triggers;
pub mod watch;
pub mod worker;

#[cfg(test)]
pub mod test_env {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/test_env.rs"
    ));
}
