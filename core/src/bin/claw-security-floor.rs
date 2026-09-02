//! `claw-security-floor` — update downgrade protection helper.
//!
//! Called by the Debian maintainer scripts of every Claw OS package,
//! by the APT pre-install hook, and by operators. It never runs the
//! agent, never opens the broker socket and never loads a model
//! provider: it reads root-owned state, a signed release manifest, and
//! answers one question — may this release be installed, activated or
//! run here?

fn main() {
    // This helper is invoked from maintainer scripts as root. Nothing
    // it writes should ever be readable by other accounts by default.
    cos::storage::set_private_umask();
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    std::process::exit(cos::update::cli::main(&args));
}
