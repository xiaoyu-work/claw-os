//! `claw-agentd` — the unprivileged Claw OS agent worker.
//!
//! Spawned by `clawd` with a private job channel on fd 3, already
//! dropped to the task owner's account with supplementary groups
//! cleared and `PR_SET_NO_NEW_PRIVS` set. Runs exactly one agent task
//! and exits. See `cos::agentd` for the process and authority model.

fn main() {
    cos::storage::set_private_umask();
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    #[cfg(unix)]
    {
        cos::agentd::worker::main(&args);
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        eprintln!("claw-agentd: the agent worker requires Unix");
        std::process::exit(1);
    }
}
