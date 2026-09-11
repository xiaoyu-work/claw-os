use super::*;

fn user_arguments() -> Vec<String> {
    [
        "space in a filename.txt",
        "--literal-option",
        "",
        "--",
        "quote'\"and\\backslash",
        "$(printf not-a-command);*",
        "tab\tnewline\ncarriage\r",
        "\u{96ea}.txt",
    ]
    .map(String::from)
    .to_vec()
}

fn expected_arguments(selector: &str, args: &[String]) -> Vec<String> {
    std::iter::once(selector.to_string())
        .chain(args.iter().cloned())
        .collect()
}

fn nul_delimited(args: &[String]) -> Vec<u8> {
    args.iter()
        .flat_map(|arg| arg.bytes().chain(std::iter::once(0)))
        .collect()
}

#[test]
fn native_node_and_shell_gui_arguments_are_literal_and_ordered() {
    let args = user_arguments();
    for runtime in [Runtime::Binary, Runtime::Node, Runtime::Shell] {
        let prepared = prepare(runtime, "--gui", &args).unwrap();
        assert_eq!(prepared.entry_argv, expected_arguments("--gui", &args));
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&prepared.user_args_json).unwrap(),
            args
        );
        assert_eq!(
            prepare(runtime, "--gui", &[]).unwrap().entry_argv,
            ["--gui"]
        );
    }
}

#[test]
fn gui_selector_is_one_argument_not_a_command_line_or_executable() {
    let selector = "one literal selector";
    let prepared = prepare(Runtime::Binary, selector, &["file".to_string()]).unwrap();
    assert_eq!(prepared.entry_argv, [selector, "file"]);
    let prepared = prepare(Runtime::Binary, "bin/auxiliary", &[]).unwrap();
    assert_eq!(prepared.entry_argv, ["bin/auxiliary"]);
}

#[test]
fn gui_selector_and_argument_limits_are_checked_without_normalization() {
    assert!(prepare(Runtime::Binary, &"s".repeat(SELECTOR_BYTES), &[]).is_ok());
    for selector in [
        String::new(),
        " \t".to_string(),
        "--gui\n".to_string(),
        "--gui\0".to_string(),
        "s".repeat(SELECTOR_BYTES + 1),
    ] {
        assert!(prepare(Runtime::Binary, &selector, &[]).is_err());
    }
    assert!(prepare(
        Runtime::Binary,
        "--gui",
        &vec![String::new(); MAX_ARGUMENTS]
    )
    .is_ok());
    assert!(prepare(
        Runtime::Binary,
        "--gui",
        &vec![String::new(); MAX_ARGUMENTS + 1]
    )
    .is_err());
    assert!(prepare(Runtime::Binary, "--gui", &["x".repeat(ARGUMENT_BYTES)]).is_ok());
    for argument in [
        "x".repeat(ARGUMENT_BYTES + 1),
        "\0".to_string(),
        "\u{1b}".to_string(),
    ] {
        assert!(prepare(Runtime::Binary, "--gui", &[argument]).is_err());
    }
}

#[test]
fn gui_argument_environment_limit_counts_encoded_bytes() {
    let mut args = vec!["x".repeat(ARGUMENT_BYTES); 15];
    args.push("x".repeat(MAX_STRUCTURED_STRING_BYTES - 49 - 15 * ARGUMENT_BYTES));
    let prepared = prepare(Runtime::Binary, "--gui", &args).unwrap();
    assert_eq!(prepared.user_args_json.len(), MAX_STRUCTURED_STRING_BYTES);
    args.last_mut().unwrap().push('x');
    assert!(prepare(Runtime::Binary, "--gui", &args).is_err());
    assert!(prepare(Runtime::Python, "--gui", &args).is_err());
}

#[test]
fn python_gui_does_not_gain_a_second_argv_dispatch() {
    let args = user_arguments();
    let prepared = prepare(Runtime::Python, "--gui", &args).unwrap();
    assert!(prepared.entry_argv.is_empty());
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&prepared.user_args_json).unwrap(),
        args
    );
}

#[cfg(unix)]
#[test]
fn actual_native_process_receives_the_gui_selector_and_user_arguments() {
    let scratch = tempfile::tempdir().unwrap();
    let source = scratch.path().join("gui_argv_probe.rs");
    let binary = scratch.path().join("gui_argv_probe");
    std::fs::write(
        &source,
        r#"
use std::io::Write;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("--gui") {
        std::process::exit(64);
    }
    let mut stdout = std::io::stdout().lock();
    for arg in args {
        stdout.write_all(arg.as_bytes()).unwrap();
        stdout.write_all(&[0]).unwrap();
    }
}
"#,
    )
    .unwrap();
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let compiled = std::process::Command::new(compiler)
        .args(["--edition=2021", "--crate-name", "gui_argv_probe"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("run Rust compiler for the private native argument fixture");
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let args = user_arguments();
    let prepared = prepare(Runtime::Binary, "--gui", &args).unwrap();
    let output = std::process::Command::new(&binary)
        .args(&prepared.entry_argv)
        .env_clear()
        .env("COS_APP_GUI", "1")
        .env("COS_COMMAND", "--gui")
        .env("COS_ARGS_JSON", &prepared.user_args_json)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.status);
    assert_eq!(
        output.stdout,
        nul_delimited(&expected_arguments("--gui", &args))
    );
}

#[cfg(unix)]
#[test]
fn actual_shell_entry_receives_literal_gui_arguments_after_its_script_path() {
    let scratch = tempfile::tempdir().unwrap();
    let script = scratch.path().join("main.sh");
    std::fs::write(&script, "printf '%s\\0' \"$@\"\n").unwrap();
    let args = user_arguments();
    let prepared = prepare(Runtime::Shell, "--gui", &args).unwrap();
    let output = std::process::Command::new("bash")
        .arg(&script)
        .args(&prepared.entry_argv)
        .env_clear()
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        nul_delimited(&expected_arguments("--gui", &args))
    );
}

#[cfg(unix)]
#[test]
fn actual_python_gui_retains_the_run_selector_args_contract() {
    let scratch = tempfile::tempdir().unwrap();
    let main = scratch.path().join("main.py");
    std::fs::write(
        &main,
        "def run(command, args):\n    return {'selector': command, 'args': args}\n",
    )
    .unwrap();
    let args = user_arguments();
    let prepared = prepare(Runtime::Python, "--gui", &args).unwrap();
    let wrapper = super::super::python_wrapper(
        &main,
        "--gui",
        &args,
        scratch.path().to_str().unwrap(),
        scratch.path().to_str().unwrap(),
    )
    .unwrap();
    let output = std::process::Command::new("python3")
        .args(["-I", "-S", "-c", &wrapper])
        .args(&prepared.entry_argv)
        .env_clear()
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"selector": "--gui", "args": args})
    );
}
