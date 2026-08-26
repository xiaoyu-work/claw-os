use super::*;

fn tempdir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cos-shell-hooks-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn shell_parse_accepts_three_known_dialects() {
    assert_eq!(Shell::parse("bash").unwrap(), Shell::Bash);
    assert_eq!(Shell::parse("ZSH").unwrap(), Shell::Zsh);
    assert_eq!(Shell::parse(" fish ").unwrap(), Shell::Fish);
}

#[test]
fn shell_parse_errs_on_unknown() {
    let err = Shell::parse("powershell").unwrap_err();
    assert!(err.contains("powershell"));
}

#[test]
fn render_bash_includes_trap_and_prompt_command() {
    let s = render_init(Shell::Bash);
    assert!(s.contains("trap '__cos_pre_exec' DEBUG"));
    assert!(s.contains("PROMPT_COMMAND"));
    // Hooks route through the `__cos_call` helper, so the literal
    // call site reads `__cos_call agent shell-hooks record-pre`.
    assert!(s.contains("agent shell-hooks record-pre"));
    assert!(s.contains("agent shell-hooks record-post"));
}

#[test]
fn render_init_uses_timeout_wrapper_in_all_dialects() {
    // Regression for the "stalled cos blocks the user's shell"
    // bug. Every dialect must guard its `cos` invocation with
    // a timeout wrapper.
    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let s = render_init(shell);
        assert!(
            s.contains("command -v timeout"),
            "{} init missing timeout guard:\n{s}",
            shell.label()
        );
        assert!(
            s.contains(&format!("timeout {HOOK_TIMEOUT_SECS}")),
            "{} init missing `timeout N`:\n{s}",
            shell.label()
        );
    }
}

#[test]
fn render_zsh_uses_add_zsh_hook() {
    let s = render_init(Shell::Zsh);
    assert!(s.contains("add-zsh-hook preexec __cos_preexec"));
    assert!(s.contains("add-zsh-hook precmd __cos_precmd"));
}

#[test]
fn render_fish_uses_event_handlers() {
    let s = render_init(Shell::Fish);
    assert!(s.contains("--on-event fish_preexec"));
    assert!(s.contains("--on-event fish_postexec"));
    assert!(s.contains("$status"));
}

#[test]
fn append_then_tail_returns_records_oldest_first() {
    let dir = tempdir("rt");
    let path = dir.join("shell-hooks.jsonl");
    append_pre_at(&path, "ls -la", 1_000).unwrap();
    append_post_at(&path, 0, 1_010).unwrap();
    append_pre_at(&path, "git status", 2_000).unwrap();
    append_post_at(&path, 1, 2_050).unwrap();
    let rows = tail_at(&path, 100).unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].cmd.as_deref(), Some("ls -la"));
    assert_eq!(rows[1].exit, Some(0));
    assert_eq!(rows[2].cmd.as_deref(), Some("git status"));
    assert_eq!(rows[3].exit, Some(1));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tail_limits_to_window_size() {
    let dir = tempdir("limit");
    let path = dir.join("shell-hooks.jsonl");
    for i in 0..10 {
        append_pre_at(&path, &format!("cmd {i}"), 100 + i as u64).unwrap();
    }
    let rows = tail_at(&path, 3).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].cmd.as_deref(), Some("cmd 7"));
    assert_eq!(rows[2].cmd.as_deref(), Some("cmd 9"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tail_skips_malformed_lines() {
    let dir = tempdir("malformed");
    let path = dir.join("shell-hooks.jsonl");
    append_pre_at(&path, "ok one", 1).unwrap();
    // Append a bad line directly.
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(f, "{{not valid json").unwrap();
    append_pre_at(&path, "ok two", 2).unwrap();
    let rows = tail_at(&path, 100).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].cmd.as_deref(), Some("ok one"));
    assert_eq!(rows[1].cmd.as_deref(), Some("ok two"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tail_missing_file_is_empty() {
    let dir = tempdir("missing");
    let path = dir.join("shell-hooks.jsonl");
    let rows = tail_at(&path, 100).unwrap();
    assert!(rows.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn clear_truncates_existing_file_returns_true() {
    let dir = tempdir("clear");
    let path = dir.join("shell-hooks.jsonl");
    append_pre_at(&path, "before", 1).unwrap();
    let cleared = clear_at(&path).unwrap();
    assert!(cleared);
    let rows = tail_at(&path, 10).unwrap();
    assert!(rows.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn clear_missing_file_returns_false() {
    let dir = tempdir("clear-missing");
    let path = dir.join("shell-hooks.jsonl");
    let cleared = clear_at(&path).unwrap();
    assert!(!cleared);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn log_rotates_at_50mib() {
    // When the active log exceeds 50 MiB, the next append must
    // rotate it. We seed the file with a >50 MiB blob (one byte
    // over so the boundary check is `>` not `>=`) and verify the
    // post-append state: the active file is small and a `.1`
    // companion holds the prior contents.
    let dir = tempdir("rotate");
    let path = dir.join("shell-hooks.jsonl");
    let blob = vec![b'.'; (MAX_LOG_BYTES + 1) as usize];
    std::fs::write(&path, &blob).unwrap();
    append_pre_at(&path, "after-rotate", 9_000).unwrap();
    let active_len = std::fs::metadata(&path).unwrap().len();
    assert!(
        active_len < MAX_LOG_BYTES,
        "active log should be small post-rotate, got {active_len} bytes"
    );
    let rotated = rotated_path(&path, 1);
    assert!(rotated.exists(), "expected {} to exist", rotated.display());
    let rotated_len = std::fs::metadata(&rotated).unwrap().len();
    assert!(
        rotated_len > MAX_LOG_BYTES,
        "rotated file should hold the old payload, got {rotated_len} bytes"
    );
    // The new record must be present in the active file (i.e.
    // the rename happened before the append wrote into the
    // newly-empty file).
    let rows = tail_at(&path, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].cmd.as_deref(), Some("after-rotate"));
    std::fs::remove_dir_all(&dir).ok();
}
