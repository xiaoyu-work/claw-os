use super::*;

#[test]
fn detects_wsl_kernel_markers() {
    assert!(is_wsl_kernel(
        Some("5.15.167.4-microsoft-standard-WSL2"),
        None
    ));
    assert!(is_wsl_kernel(None, Some("Linux version with WSL marker")));
    assert!(!is_wsl_kernel(Some("6.8.0-generic"), Some("Linux version")));
}

#[test]
fn parses_proc_stat_fields_after_command_name() {
    let parsed = parse_proc_stat("123 (bash with spaces) S 1 2 3 34816 5 4194304");
    assert_eq!(
        parsed,
        ProcStatSummary {
            state: Some("S".to_string()),
            ppid: Some(1),
            pgrp: Some(2),
            session: Some(3),
            tty_nr: Some(34816),
        }
    );
}

#[test]
fn parses_apt_history_block() {
    let event = parse_apt_history_block(
        Path::new("/var/log/apt/history.log"),
        "Start-Date: 2026-05-17  12:00:00\nCommandline: apt install git\nInstall: git:amd64\nEnd-Date: 2026-05-17  12:00:02\n",
    )
    .expect("apt history event");

    assert_eq!(event["commandline"], "apt install git");
    assert_eq!(event["install"], "git:amd64");
}

#[test]
fn skips_expensive_recent_file_directories() {
    assert!(skip_recent_dir(".git"));
    assert!(skip_recent_dir("node_modules"));
    assert!(!skip_recent_dir("Downloads"));
}
