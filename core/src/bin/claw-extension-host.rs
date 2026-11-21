//! Unprivileged host for task-scoped App and MCP processes.

fn main() {
    yu::storage::set_private_umask();
    if let Err(refusal) =
        yu::update::runtime::enforce_startup(yu::update::runtime::Scope::CompiledEpoch)
    {
        eprintln!("claw-extension-host: {}", refusal.message);
        std::process::exit(1);
    }
    #[cfg(unix)]
    {
        yu::extension_host::host::main();
    }
    #[cfg(not(unix))]
    {
        eprintln!("claw-extension-host: the extension host requires Unix");
        std::process::exit(1);
    }
}
