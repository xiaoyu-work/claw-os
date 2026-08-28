fn main() {
    if let Err(error) = cos_runtime::ask_claw::run_sdk_launcher() {
        eprintln!("Ask Claw launcher failed: {error}");
        std::process::exit(1);
    }
}
