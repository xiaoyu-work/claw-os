use super::*;
use serde_json::Value;
fn dev_cmd(command: &str, args: &[String]) -> Result<Value, String> {
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push(command.to_string());
    full.extend_from_slice(args);
    run("dev", &full)
}
macro_rules! root_wrapper {
    ($name:ident, $command:literal) => {
        fn $name(args: &[String]) -> Result<Value, String> {
            dev_cmd($command, args)
        }
    };
}
root_wrapper!(insights_cmd, "insights");
root_wrapper!(llm_cmd, "llm");
root_wrapper!(prompt_cmd, "prompt");
root_wrapper!(nudge_cmd, "nudge");
root_wrapper!(usage_cmd, "usage");
root_wrapper!(curator_cmd, "curator");
root_wrapper!(tools_cmd, "tools");
root_wrapper!(guardrails_cmd, "guardrails");
root_wrapper!(approval_cmd, "approval");
root_wrapper!(compress_cmd, "compress");
root_wrapper!(aux_cmd, "aux");
root_wrapper!(retry_cmd, "retry");
root_wrapper!(semantic_cmd, "semantic");
root_wrapper!(vision_cmd, "vision");
root_wrapper!(display_cmd, "display");
root_wrapper!(shell_hooks_cmd, "shell-hooks");
root_wrapper!(media_cmd, "media");
root_wrapper!(binary_ext_cmd, "binary-ext");
root_wrapper!(context_cmd, "context");
root_wrapper!(file_safety_cmd, "file-safety");
root_wrapper!(osv_cmd, "osv");
root_wrapper!(interrupt_cmd, "interrupt");
root_wrapper!(learn_cmd, "learn");
root_wrapper!(hooks_cmd, "hooks");
root_wrapper!(providers_cmd, "providers");
root_wrapper!(provider_doctor_cmd, "provider-doctor");
root_wrapper!(redact_cmd, "redact");
root_wrapper!(think_scrub_cmd, "think-scrub");
root_wrapper!(tokens_cmd, "tokens");
root_wrapper!(summarise_cmd, "summarise");
root_wrapper!(classify_cmd, "classify");
fn budget_cmd(args: &[String]) -> Result<Value, String> {
    run("budget", args)
}
fn chat_cmd(args: &[String]) -> Result<Value, String> {
    run("chat", args)
}
fn mcp_cmd(args: &[String]) -> Result<Value, String> {
    run("mcp", args)
}
fn notes_cmd(args: &[String]) -> Result<Value, String> {
    run("notes", args)
}
fn sessions_cmd(args: &[String]) -> Result<Value, String> {
    run("sessions", args)
}
fn skills_cmd(args: &[String]) -> Result<Value, String> {
    run("skills", args)
}
fn parse_set_title_args(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["set-title".into()];
    a.extend_from_slice(args);
    sessions_cmd(&a)
}
fn sessions_title(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["title".into()];
    a.extend_from_slice(args);
    sessions_cmd(&a)
}
fn sessions_stats(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["stats".into()];
    a.extend_from_slice(args);
    sessions_cmd(&a)
}
fn curator_drafts_cmd(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["drafts".into()];
    a.extend_from_slice(args);
    curator_cmd(&a)
}
fn vision_route_cmd(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["route".into()];
    a.extend_from_slice(args);
    vision_cmd(&a)
}
fn vision_sniff_cmd(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["sniff".into()];
    a.extend_from_slice(args);
    vision_cmd(&a)
}
fn vision_analyze_cmd(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["analyze".into()];
    a.extend_from_slice(args);
    vision_cmd(&a)
}
fn display_format_bytes_cmd(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["format-bytes".into()];
    a.extend_from_slice(args);
    display_cmd(&a)
}
fn display_format_duration_cmd(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["format-duration".into()];
    a.extend_from_slice(args);
    display_cmd(&a)
}
fn parse_display_transcript_args(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["transcript".into()];
    a.extend_from_slice(args);
    display_cmd(&a)
}
fn context_hints_cmd(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["hints".into()];
    a.extend_from_slice(args);
    context_cmd(&a)
}
fn context_refs_cmd(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["refs".into()];
    a.extend_from_slice(args);
    context_cmd(&a)
}
fn media_play_cmd(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["play".into()];
    a.extend_from_slice(args);
    media_cmd(&a)
}
fn media_playback_status_cmd(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["playback-status".into()];
    a.extend_from_slice(args);
    media_cmd(&a)
}
fn parse_mcp_spawn_spec(args: &[String]) -> Result<Value, String> {
    let mut a = vec!["probe".into()];
    a.extend_from_slice(args);
    mcp_cmd(&a)
}
fn read_text_input(args: &[String], _: &str) -> Result<Value, String> {
    tokens_cmd(args)
}
fn skills_usage_cmd_at(args: &[String], _: &std::path::Path) -> Result<Value, String> {
    let mut a = vec!["usage".into()];
    a.extend_from_slice(args);
    run("skills", &a)
}
fn skills_guard_cmd_against<T>(args: &[String], _: &T) -> Result<Value, String> {
    let mut a = vec!["guard".into()];
    a.extend_from_slice(args);
    run("skills", &a)
}
fn todo_cmd_at(args: &[String], _: &crate::agent::tools::todo::TodoStore) -> Result<Value, String> {
    run("todo", args)
}

// ----------------------------------------------------------------
// CLI dispatch contract
//
// Every subcommand dispatcher must reject bad input with an error
// that tells the user what to do instead. That contract used to be
// covered by ~120 near-identical 4-line tests; the tables below
// assert the same thing per-dispatcher without the duplication.
// ----------------------------------------------------------------

/// One row of a CLI-rejection table: a label for failure output, the
/// invocation under test, and substrings the error must mention.
type CliCase = (
    &'static str,
    Box<dyn Fn() -> Result<(), String>>,
    Vec<&'static str>,
);

/// Build a [`CliCase`]. The call is normalised to `Result<(), String>`
/// so dispatchers with different `Ok` types share one table.
macro_rules! cli_case {
    ($label:expr, $call:expr, [$($want:expr),* $(,)?]) => {
        (
            $label,
            Box::new(move || $call.map(|_| ())) as Box<dyn Fn() -> Result<(), String>>,
            vec![$($want),*],
        )
    };
}

/// Assert every case errors and that the message mentions each
/// expected substring. Matching is case-insensitive because some
/// dispatchers capitalise their usage banner.
fn assert_cli_rejects(cases: Vec<CliCase>) {
    for (label, invoke, expected) in cases {
        let err = match invoke() {
            Err(e) => e,
            Ok(()) => panic!("{label}: expected an error, got Ok"),
        };
        let hay = err.to_lowercase();
        for want in expected {
            assert!(
                hay.contains(&want.to_lowercase()),
                "{label}: error {err:?} should mention {want:?}"
            );
        }
    }
}

#[test]
fn cli_unknown_subcommand_lists_available_options() {
    assert_cli_rejects(vec![
        cli_case!(
            "agent root",
            run("not-a-command", &[]),
            ["ask", "setup", "sessions", "override", "dev"]
        ),
        cli_case!(
            "budget user",
            budget_cmd(&["user".into(), "bogus".into()]),
            ["bogus", "show", "path"]
        ),
        cli_case!(
            "insights",
            insights_cmd(&["bogus".into()]),
            ["bogus", "overall"]
        ),
        cli_case!(
            "notes",
            notes_cmd(&["bogus".into()]),
            ["list", "read", "write"]
        ),
        cli_case!(
            "skills",
            skills_cmd(&["bogus".into()]),
            ["list", "info", "disabled"]
        ),
        cli_case!(
            "skills hub (no subcommand)",
            skills_cmd(&["hub".into()]),
            ["list", "install"]
        ),
        cli_case!(
            "skills hub",
            skills_cmd(&["hub".into(), "bogus".into(), "owner/repo".into()]),
            ["list", "install"]
        ),
        cli_case!("llm", llm_cmd(&["bogus".into()]), ["providers", "models"]),
        cli_case!(
            "skills usage",
            {
                let dir = tempfile::tempdir().expect("tmp");
                let p = dir.path().join("usage.jsonl");
                skills_usage_cmd_at(&["bogus".into()], &p)
            },
            ["stats", "record"]
        ),
        cli_case!("prompt", prompt_cmd(&["bogus".into()]), ["show", "build"]),
        cli_case!(
            "nudge",
            nudge_cmd(&["bogus".into()]),
            ["list", "add", "fire"]
        ),
        cli_case!("mcp", mcp_cmd(&["bogus".into()]), ["status", "serve"]),
        cli_case!(
            "usage scope",
            usage_cmd(&["bogus".into()]),
            ["provider", "model", "session", "app", "verb"]
        ),
        cli_case!(
            "curator",
            curator_cmd(&["bogus".into()]),
            ["propose", "scan", "author"]
        ),
        cli_case!(
            "curator drafts",
            curator_drafts_cmd(&["bogus".into()]),
            ["auto-title", "retitle"]
        ),
        cli_case!("tools", tools_cmd(&["bogus".into()]), ["bogus", "list"]),
        cli_case!(
            "guardrails",
            guardrails_cmd(&["bogus".into()]),
            ["bogus", "show"]
        ),
        cli_case!(
            "approval",
            approval_cmd(&["bogus".into()]),
            ["bogus", "show"]
        ),
        cli_case!("todo", run("todo", &["bogus".into()]), ["bogus"]),
        cli_case!("compress", compress_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!("aux", aux_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!("retry", retry_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!("sessions", sessions_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!(
            "semantic",
            semantic_cmd(&["bogus".into()]),
            ["clear-all", "status"]
        ),
        cli_case!("vision", vision_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!("display", display_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!(
            "shell hooks",
            shell_hooks_cmd(&["bogus".into()]),
            ["bogus", "init"]
        ),
        cli_case!(
            "media",
            media_cmd(&["bogus".into()]),
            ["bogus", "providers"]
        ),
        cli_case!(
            "binary ext",
            binary_ext_cmd(&["bogus".into()]),
            ["bogus", "list"]
        ),
        cli_case!(
            "context",
            context_cmd(&["bogus".into()]),
            ["bogus", "hints"]
        ),
        cli_case!("file safety", file_safety_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!("osv", osv_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!(
            "interrupt",
            interrupt_cmd(&["frobnicate".into()]),
            ["unknown"]
        ),
        cli_case!("learn", learn_cmd(&["frobnicate".into()]), ["unknown"]),
        cli_case!("hooks", hooks_cmd(&["frobnicate".into()]), ["unknown"]),
    ]);
}

#[test]
fn cli_unknown_flag_is_rejected() {
    assert_cli_rejects(vec![
        cli_case!(
            "prompt show",
            prompt_cmd(&["show".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "mcp serve",
            mcp_cmd(&["serve".into(), "--bogus".into(), "x".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "curator propose",
            curator_cmd(&["propose".into(), "any-sid".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "curator author",
            curator_cmd(&["author".into(), "draft-1".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "curator scan",
            curator_cmd(&["scan".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "curator drafts auto-title",
            curator_drafts_cmd(&["auto-title".into(), "some-id".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "providers",
            providers_cmd(&["--bogus".into()]),
            ["--bogus", "--names"]
        ),
        cli_case!(
            "provider doctor",
            provider_doctor_cmd(&["--mystery".into()]),
            ["--mystery", "--probe-network"]
        ),
        cli_case!(
            "approval check",
            approval_cmd(&["check".into(), "echo".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "compress check",
            compress_cmd(&["check".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "mcp spawn spec",
            parse_mcp_spawn_spec(&["--cmd".into(), "x".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "aux ask",
            aux_cmd(&[
                "ask".into(),
                "--prompt".into(),
                "hi".into(),
                "--bogus".into(),
            ]),
            ["--bogus"]
        ),
        cli_case!(
            "retry schedule",
            retry_cmd(&["schedule".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "skills guard",
            skills_cmd(&["guard".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "vision route",
            vision_route_cmd(&["--bytes".into(), "1024".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "vision sniff",
            vision_sniff_cmd(&["--bogus".into(), "x".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "vision analyze",
            vision_analyze_cmd(&[
                "--bogus".into(),
                "v".into(),
                "--file".into(),
                "x.png".into(),
                "--prompt".into(),
                "describe".into(),
            ]),
            ["--bogus"]
        ),
        cli_case!(
            "display transcript",
            parse_display_transcript_args(&["--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "shell hooks tail",
            shell_hooks_cmd(&["tail".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "media list-outputs",
            media_cmd(&["list-outputs".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "binary ext list",
            binary_ext_cmd(&["list".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "context hints",
            context_hints_cmd(&["--bogus".into(), "x".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "context refs",
            context_refs_cmd(&["--bogus".into(), "v".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "context build",
            context_cmd(&["build".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "osv query",
            osv_cmd(&[
                "query".into(),
                "foo@1.0".into(),
                "--bogus".into(),
                "x".into(),
            ]),
            ["--bogus"]
        ),
        // `ask` must enumerate supported flags so users can discover
        // `--full` without reading the source.
        cli_case!(
            "ask",
            run("ask", &["--bogus".into(), "hi".into()]),
            ["unknown ask flag", "--full"]
        ),
        cli_case!(
            "ask stream removed",
            run("ask", &["--stream".into(), "hi".into()]),
            ["unknown ask flag", "--stream", "--full"]
        ),
        cli_case!("chat", chat_cmd(&["--bogus".into()]), ["unknown flag"]),
        cli_case!(
            "learn extract",
            learn_cmd(&["extract".into(), "--frobnicate".into(), "x".into()]),
            ["unknown"]
        ),
        cli_case!(
            "media play",
            media_play_cmd(&["--frobnicate".into(), "a.wav".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "media playback-status",
            media_playback_status_cmd(&["--quack".into()]),
            ["unknown flag"]
        ),
    ]);
}

#[test]
fn cli_missing_required_argument_reports_usage() {
    assert_cli_rejects(vec![
        cli_case!("notes read", notes_cmd(&["read".into()]), ["usage"]),
        cli_case!("skills info", skills_cmd(&["info".into()]), ["usage"]),
        cli_case!(
            "skills hub list",
            skills_cmd(&["hub".into(), "list".into()]),
            ["owner/repo"]
        ),
        cli_case!(
            "skills hub install",
            skills_cmd(&["hub".into(), "install".into(), "owner/repo".into()]),
            ["usage:", "install"]
        ),
        cli_case!(
            "skills hub show",
            skills_cmd(&["hub".into(), "show".into(), "owner/repo".into()]),
            ["usage:", "show"]
        ),
        cli_case!("redact", redact_cmd(&[]), ["usage:"]),
        cli_case!(
            "skills usage record",
            {
                let dir = tempfile::tempdir().expect("tmp");
                let p = dir.path().join("usage.jsonl");
                skills_usage_cmd_at(&["record".into()], &p)
            },
            ["usage:"]
        ),
        cli_case!("think-scrub", think_scrub_cmd(&[]), ["usage:"]),
        cli_case!("tokens", tokens_cmd(&[]), ["usage:"]),
        cli_case!("nudge add", nudge_cmd(&["add".into()]), ["usage"]),
        cli_case!(
            "nudge add (due only)",
            nudge_cmd(&["add".into(), "30".into()]),
            ["usage"]
        ),
        cli_case!("nudge fire", nudge_cmd(&["fire".into()]), ["usage"]),
        cli_case!("usage provider", usage_cmd(&["provider".into()]), ["usage"]),
        cli_case!("usage model", usage_cmd(&["model".into()]), ["usage"]),
        cli_case!("usage session", usage_cmd(&["session".into()]), ["usage"]),
        cli_case!("usage app", usage_cmd(&["app".into()]), ["usage"]),
        cli_case!("usage verb", usage_cmd(&["verb".into()]), ["usage"]),
        cli_case!(
            "curator drafts auto-title",
            curator_drafts_cmd(&["auto-title".into()]),
            ["usage"]
        ),
        cli_case!("tools show", tools_cmd(&["show".into()]), ["show"]),
        cli_case!("set-title", parse_set_title_args(&[]), ["usage"]),
        cli_case!("sessions title", sessions_title(&[]), ["usage"]),
        cli_case!("display", display_cmd(&[]), ["usage"]),
        cli_case!(
            "display format-bytes",
            display_format_bytes_cmd(&[]),
            ["usage"]
        ),
        cli_case!(
            "display format-duration",
            display_format_duration_cmd(&[]),
            ["usage"]
        ),
        cli_case!(
            "shell hooks record-post",
            shell_hooks_cmd(&["record-post".into()]),
            ["usage"]
        ),
        cli_case!(
            "binary ext check",
            binary_ext_cmd(&["check".into()]),
            ["usage"]
        ),
        cli_case!("context", context_cmd(&[]), ["usage"]),
        cli_case!("file safety", file_safety_cmd(&[]), ["usage", "check"]),
        cli_case!(
            "file safety check",
            file_safety_cmd(&["check".into()]),
            ["usage"]
        ),
        cli_case!("osv", osv_cmd(&[]), ["usage", "parse"]),
        cli_case!("osv parse", osv_cmd(&["parse".into()]), ["usage"]),
    ]);
}

#[test]
fn cli_flag_without_value_names_the_flag() {
    assert_cli_rejects(vec![
        cli_case!("redact --file", redact_cmd(&["--file".into()]), ["--file"]),
        cli_case!(
            "prompt --extra",
            prompt_cmd(&["show".into(), "--extra".into()]),
            ["--extra"]
        ),
        cli_case!(
            "read_text_input --file",
            read_text_input(&["--file".into()], "tokens"),
            ["--file"]
        ),
        cli_case!(
            "usage --app",
            usage_cmd(&["overall".into(), "--app".into()]),
            ["--app"]
        ),
        cli_case!(
            "usage --verb",
            usage_cmd(&["overall".into(), "--verb".into()]),
            ["--verb"]
        ),
        cli_case!(
            "mcp serve --allow",
            mcp_cmd(&["serve".into(), "--allow".into()]),
            ["--allow"]
        ),
        cli_case!(
            "mcp serve --deny",
            mcp_cmd(&["serve".into(), "--deny".into()]),
            ["--deny"]
        ),
        cli_case!(
            "curator propose --min-turns",
            curator_cmd(&["propose".into(), "any-sid".into(), "--min-turns".into()]),
            ["--min-turns"]
        ),
        cli_case!(
            "curator scan --limit",
            curator_cmd(&["scan".into(), "--limit".into()]),
            ["--limit"]
        ),
        cli_case!(
            "providers --names",
            providers_cmd(&["--names".into()]),
            ["--names"]
        ),
        cli_case!(
            "provider doctor --names",
            provider_doctor_cmd(&["--names".into()]),
            ["--names"]
        ),
        cli_case!(
            "summarise --max",
            summarise_cmd(&["--max".into()]),
            ["--max"]
        ),
        cli_case!(
            "classify --labels",
            classify_cmd(&["--labels".into()]),
            ["--labels"]
        ),
        cli_case!(
            "approval check --input",
            approval_cmd(&["check".into(), "echo".into(), "--input".into()]),
            ["--input"]
        ),
        cli_case!(
            "sessions stats --session",
            sessions_stats(&["--session".into()]),
            ["--session requires"]
        ),
        cli_case!(
            "shell hooks tail --limit",
            shell_hooks_cmd(&["tail".into(), "--limit".into()]),
            ["--limit"]
        ),
        cli_case!(
            "media list-outputs --limit",
            media_cmd(&["list-outputs".into(), "--limit".into()]),
            ["--limit"]
        ),
        cli_case!(
            "media list-outputs --ext",
            media_cmd(&["list-outputs".into(), "--ext".into()]),
            ["--ext"]
        ),
        cli_case!(
            "binary ext list --limit",
            binary_ext_cmd(&["list".into(), "--limit".into()]),
            ["--limit"]
        ),
        cli_case!(
            "chat --session",
            chat_cmd(&["--session".into()]),
            ["--session"]
        ),
        cli_case!(
            "chat --max-turns",
            chat_cmd(&["--max-turns".into()]),
            ["--max-turns"]
        ),
        cli_case!(
            "media playback-status --format",
            media_playback_status_cmd(&["--format".into()]),
            ["--format"]
        ),
    ]);
}
