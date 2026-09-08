fn python_arguments(
    mut native_args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<Vec<std::ffi::OsString>, &'static str> {
    let caller = native_args.next();
    if caller.as_deref() != Some(std::ffi::OsStr::new("claw-mail-ai@claw.os"))
        || native_args.next().is_some()
    {
        return Err("expected only the trusted Thunderbird extension identity");
    }
    Ok(vec![
        std::ffi::OsString::from("-I"),
        std::ffi::OsString::from("/usr/lib/cos/apps/mail-ai/native_host.py"),
    ])
}

fn main() {
    let args = match python_arguments(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("claw-mail-ai-host: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = cos::bridge::run_native_app_host(
        "mail-ai",
        std::path::Path::new("/usr/lib/cos/apps/mail-ai"),
        std::ffi::OsStr::new("/usr/bin/python3"),
        &args,
    ) {
        eprintln!("claw-mail-ai-host: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/bin/claw-mail-ai-host.rs"
    ));
}
