//! Unprivileged host for task-scoped App and MCP processes.

fn main() {
    cos::storage::set_private_umask();
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
