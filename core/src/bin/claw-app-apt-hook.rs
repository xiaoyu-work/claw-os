fn main() {
    if let Err(refusal) =
        cos::update::runtime::enforce_startup(cos::update::runtime::Scope::CompiledEpoch)
    {
        eprintln!("claw-app-apt-hook: {}", refusal.message);
        std::process::exit(1);
    }
    if let Err(error) = cos::clawd::app_store::run_apt_hook() {
        eprintln!("claw-app-apt-hook: {error}");
        std::process::exit(1);
    }
}
