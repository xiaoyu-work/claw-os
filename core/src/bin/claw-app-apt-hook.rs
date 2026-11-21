fn main() {
    if let Err(refusal) =
        yu::update::runtime::enforce_startup(yu::update::runtime::Scope::CompiledEpoch)
    {
        eprintln!("claw-app-apt-hook: {}", refusal.message);
        std::process::exit(1);
    }
    if let Err(error) = yu::clawd::app_store::run_apt_hook() {
        eprintln!("claw-app-apt-hook: {error}");
        std::process::exit(1);
    }
}
