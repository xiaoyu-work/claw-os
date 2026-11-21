//! `claw-agentd` — the unprivileged Claw OS agent worker.
//!
//! Spawned by `clawd` with a private job channel on fd 3, already
//! dropped to the task owner's account with supplementary groups
//! cleared and `PR_SET_NO_NEW_PRIVS` set. Runs exactly one agent task
//! and exits. See `yu::agentd` for the process and authority model.

fn main() {
    yu::storage::set_private_umask();
    if let Err(refusal) =
        yu::update::runtime::enforce_startup(yu::update::runtime::Scope::CompiledEpoch)
    {
        eprintln!("claw-agentd: {refusal}");
        std::process::exit(1);
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    #[cfg(unix)]
    {
        yu::agentd::worker::main(&args);
    }
    #[cfg(not(unix))]
    {
        let _ = args;
        eprintln!("claw-agentd: the agent worker requires Unix");
        std::process::exit(1);
    }
}
