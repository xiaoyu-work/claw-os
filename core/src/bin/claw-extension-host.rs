//! Unprivileged host for task-scoped App and MCP processes.

fn main() {
    cos::storage::set_private_umask();
    if let Err(refusal) =
        cos::update::runtime::enforce_startup(cos::update::runtime::Scope::CompiledEpoch)
    {
        eprintln!("claw-extension-host: {}", refusal.message);
        std::process::exit(1);
    }
    #[cfg(unix)]
    {
        cos::extension_host::host::main();
    }
    #[cfg(not(unix))]
    {
        eprintln!("claw-extension-host: the extension host requires Unix");
        std::process::exit(1);
    }
}
