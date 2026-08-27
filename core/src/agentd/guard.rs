//! Keeps the live model/tool runtime out of the root broker process.
//!
//! `clawd` and `claw-agentd` are built from the same library, so the
//! agent runtime is reachable from the broker binary by construction.
//! What matters is that it never *executes* there: the `clawd` entry
//! point calls [`mark_broker_process`] before anything else, and every
//! surface that would pull a provider HTTP client, an MCP client, a
//! model-visible tool registry or a dynamically launched App into the
//! root address space checks [`ensure_agent_runtime_allowed`] first and
//! fails closed.
//!
//! The flag is process-wide and one-way on purpose. A worker never sets
//! it, so the same code paths behave exactly as before once execution
//! has moved to the unprivileged `claw-agentd` process.

use std::sync::atomic::{AtomicBool, Ordering};

static BROKER_PROCESS: AtomicBool = AtomicBool::new(false);

/// Declare this process the privileged broker. Called once from
/// `clawd`'s `main` before the daemon opens its socket.
pub fn mark_broker_process() {
    BROKER_PROCESS.store(true, Ordering::SeqCst);
}

pub fn is_broker_process() -> bool {
    BROKER_PROCESS.load(Ordering::SeqCst)
}

/// Error text shared by every guarded surface so the failure is
/// recognisable in logs and task errors.
pub fn ensure_agent_runtime_allowed(surface: &str) -> Result<(), String> {
    if is_broker_process() {
        return Err(format!(
            "agent runtime surface `{surface}` is not available inside the clawd broker \
             process; agent work runs in the unprivileged claw-agentd worker"
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn clear_broker_process_for_test() {
    BROKER_PROCESS.store(false, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agentd/guard.rs"
    ));
}
