fn main() {
    let mut native_args = std::env::args_os().skip(1);
    let caller = native_args.next();
    if caller.as_deref() != Some(std::ffi::OsStr::new("claw-mail-ai@claw.os")) {
        eprintln!("claw-mail-ai-host: untrusted Thunderbird extension");
        std::process::exit(1);
    }
    let mut args = vec![
        std::ffi::OsString::from("-I"),
        std::ffi::OsString::from("/usr/lib/cos/apps/mail-ai/native_host.py"),
        std::ffi::OsString::from("claw-mail-ai@claw.os"),
    ];
    args.extend(native_args);
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
