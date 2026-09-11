fn main() {
    cos::storage::set_private_umask();
    if let Err(refusal) =
        cos::update::runtime::enforce_startup(cos::update::runtime::Scope::CompiledEpoch)
    {
        eprintln!("claw-display-host: {}", refusal.message);
        std::process::exit(1);
    }
    #[cfg(target_os = "linux")]
    let result = cos::display_session::host::run();
    #[cfg(not(target_os = "linux"))]
    let result: Result<(), String> = Err("display activation requires Linux".to_string());
    if let Err(error) = result {
        eprintln!("claw-display-host: {error}");
        std::process::exit(1);
    }
}
