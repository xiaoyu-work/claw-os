//! Shell integration scripts.
//!
//! Port of `aterm-local/src/shellIntegration.ts`. Generates per-shell snippets
//! emitting OSC 133 markers and defining the `__cos_ai` function which:
//!   1. Overwrites the echo line in the alacritty grid with `@ <prompt>`.
//!   2. `exec`s `copilot -p "<prompt with context>" --allow-all-tools`.
//!
//! The user's shell sources these via shell-specific mechanisms — we write
//! them to a cache dir and configure the shell's startup flags / env in the
//! PTY spawn path (`tty::Options`).
//!
//! ## Auto-source mechanism per shell
//!
//! - **bash**: spawn with `--rcfile <our wrapper>` where the wrapper first
//!   sources our integration then `$HOME/.bashrc`.
//! - **zsh**: set `ZDOTDIR=<our dotdir>` containing a `.zshrc` that sources
//!   the user's real `$HOME/.zshrc` (or `$ZDOTDIR/.zshrc` if previously set)
//!   followed by our integration.
//! - **fish**: spawn with `--init-command='source <our integration>'`.
//! - **pwsh**: pass `-NoExit -Command "& '<integration.ps1>'"` plus user
//!   profile sourcing.
//!
//! For login shells we accept that the integration is sourced *after* the
//! user's profile, which is what aterm does too.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Paths to integration scripts on disk.
#[derive(Clone, Debug)]
pub struct IntegrationDirs {
    /// Root directory under `XDG_CACHE_HOME/cosmic-term/cos-ai`.
    pub root: PathBuf,
    pub bash_init: PathBuf,
    pub bash_integration: PathBuf,
    pub zsh_dotdir: PathBuf,
    pub zsh_integration: PathBuf,
    pub fish_integration: PathBuf,
    pub pwsh_integration: PathBuf,
    pub cmd_stub: PathBuf,
}

impl IntegrationDirs {
    fn under(root: PathBuf) -> Self {
        Self {
            bash_init: root.join("bash-init.sh"),
            bash_integration: root.join("integration.bash"),
            zsh_dotdir: root.join("zdotdir"),
            zsh_integration: root.join("integration.zsh"),
            fish_integration: root.join("integration.fish"),
            pwsh_integration: root.join("integration.ps1"),
            cmd_stub: root.join("__cos_ai.cmd"),
            root,
        }
    }
}

/// Write all integration scripts to disk (idempotent). Returns paths.
///
/// `cache_dir` is `XDG_CACHE_HOME/cosmic-term` (or fallback `~/.cache/cosmic-term`).
/// `copilot_bin` is the absolute path to the `copilot` executable (defaults to
/// `"copilot"`, looked up via `$PATH` by the shell).
pub fn ensure_integration_dirs(
    cache_dir: &Path,
    copilot_bin: &str,
    extra_args: &str,
    allow_all_tools: bool,
    model: &str,
) -> io::Result<IntegrationDirs> {
    let root = cache_dir.join("cos-ai");
    fs::create_dir_all(&root)?;
    let dirs = IntegrationDirs::under(root);

    let copilot_flags = build_copilot_flags(allow_all_tools, model, extra_args);

    fs::write(&dirs.bash_init, BASH_RCFILE_WRAPPER)?;
    fs::write(&dirs.bash_integration, bash_integration(&copilot_flags, copilot_bin))?;

    fs::create_dir_all(&dirs.zsh_dotdir)?;
    fs::write(dirs.zsh_dotdir.join(".zshrc"), ZSH_DOTDIR_WRAPPER)?;
    fs::write(&dirs.zsh_integration, zsh_integration(&copilot_flags, copilot_bin))?;

    fs::write(&dirs.fish_integration, fish_integration(&copilot_flags, copilot_bin))?;
    fs::write(&dirs.pwsh_integration, pwsh_integration(&copilot_flags, copilot_bin))?;
    fs::write(&dirs.cmd_stub, CMD_STUB)?;

    Ok(dirs)
}

fn build_copilot_flags(allow_all_tools: bool, model: &str, extra_args: &str) -> String {
    let mut parts = Vec::new();
    if allow_all_tools {
        parts.push("--allow-all-tools".to_string());
    }
    if !model.is_empty() {
        // Shell-quote the model name; copilot accepts `--model gpt-5` etc.
        parts.push(format!("--model {}", shell_escape(model)));
    }
    let extra = extra_args.trim();
    if !extra.is_empty() {
        parts.push(extra.to_string());
    }
    parts.join(" ")
}

/// Minimal POSIX shell escape. Wraps a value in single quotes, escaping any
/// embedded single quotes via the `'\''` trick. Good enough for user-supplied
/// model names / arg strings; not a general-purpose shell quote.
fn shell_escape(value: &str) -> String {
    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'+' | b'=' | b':'))
    {
        return value.to_string();
    }
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

// ────────────────────────── Bash ──────────────────────────

const BASH_RCFILE_WRAPPER: &str = r#"# cosmic-term AI integration wrapper.
# Loaded via `bash --rcfile <this file>`. We source the user's real bashrc
# then layer our AI integration on top.
if [ -r "$HOME/.bashrc" ]; then
    . "$HOME/.bashrc"
fi
if [ -n "$COS_AI_INTEGRATION_DIR" ] && [ -r "$COS_AI_INTEGRATION_DIR/integration.bash" ]; then
    . "$COS_AI_INTEGRATION_DIR/integration.bash"
fi
"#;

fn bash_integration(copilot_flags: &str, copilot_bin: &str) -> String {
    format!(
        r#"# cosmic-term AI integration — auto-generated, do not edit.
if [ -n "$__COS_TERM_SHELL_INTEGRATION_ACTIVE" ]; then return; fi
__COS_TERM_SHELL_INTEGRATION_ACTIVE=1

# OSC 133 command boundary markers (Marked command lines / output / exit code).
__cos_term_precmd() {{
    local ec=$?
    printf '\e]133;D;%d\a' "$ec"
    printf '\e]133;A\a'
}}
__cos_term_preexec() {{
    printf '\e]133;C\a'
}}
case "$PROMPT_COMMAND" in
    *__cos_term_precmd*) ;;
    *) PROMPT_COMMAND="__cos_term_precmd${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}" ;;
esac
trap '__cos_term_preexec' DEBUG

# Ensure PS1 has a 133;B marker (end of prompt / start of command input).
case "$PS1" in
    *'133;B'*) ;;
    *) PS1="${{PS1}}\[\e]133;B\a\]" ;;
esac

# Hide `__cos_ai` invocations from history (leading space).
case "$HISTCONTROL" in
    *ignorespace*|*ignoreboth*) ;;
    *) HISTCONTROL="${{HISTCONTROL:+$HISTCONTROL:}}ignorespace" ;;
esac

# AI assistant function — invoked as ` __cos_ai <queryId>` by cosmic-term.
# Reads prompt from $COS_AI_TMP/aq-$1.txt and context from $COS_AI_TMP/ac-$1.json.
__cos_ai() {{
    if [ -z "$COS_AI_TMP" ]; then
        echo "cosmic-term AI not active in this shell." >&2
        return 1
    fi
    local qfile="$COS_AI_TMP/aq-$1.txt"
    local cfile="$COS_AI_TMP/ac-$1.json"
    if [ ! -f "$qfile" ]; then
        echo "cos-ai: query file $qfile missing." >&2
        return 1
    fi
    # Overwrite shell echo of "__cos_ai <id>" with a clean "@ <prompt>" line.
    local fl
    if IFS= read -r fl < "$qfile" && [ -n "$fl" ]; then
        local trunc="$fl"
        if [ "${{#trunc}}" -gt 80 ]; then trunc="${{trunc:0:80}}..."; fi
        printf '\e[A\r\e[2K\e[36m@ \e[39m%s\n' "$trunc"
    fi
    local prompt
    if [ -f "$cfile" ]; then
        prompt="<terminal_context>$(cat "$cfile")</terminal_context>

<user_request>
$(cat "$qfile")
</user_request>"
    else
        prompt="$(cat "$qfile")"
    fi
    {copilot_bin} -p "$prompt" {copilot_flags}
    local rc=$?
    rm -f "$qfile" "$cfile"
    return $rc
}}
"#
    )
}

// ────────────────────────── Zsh ──────────────────────────

const ZSH_DOTDIR_WRAPPER: &str = r#"# cosmic-term AI integration ZDOTDIR wrapper.
# Source the user's real zshrc first (preferring an earlier $ZDOTDIR override).
if [ -n "$COS_TERM_PREV_ZDOTDIR" ] && [ -r "$COS_TERM_PREV_ZDOTDIR/.zshrc" ]; then
    ZDOTDIR="$COS_TERM_PREV_ZDOTDIR"
    source "$COS_TERM_PREV_ZDOTDIR/.zshrc"
elif [ -r "$HOME/.zshrc" ]; then
    source "$HOME/.zshrc"
fi
unset COS_TERM_PREV_ZDOTDIR
if [ -n "$COS_AI_INTEGRATION_DIR" ] && [ -r "$COS_AI_INTEGRATION_DIR/integration.zsh" ]; then
    source "$COS_AI_INTEGRATION_DIR/integration.zsh"
fi
"#;

fn zsh_integration(copilot_flags: &str, copilot_bin: &str) -> String {
    format!(
        r#"# cosmic-term AI integration — auto-generated, do not edit.
if [[ -n "$__COS_TERM_SHELL_INTEGRATION_ACTIVE" ]]; then return; fi
__COS_TERM_SHELL_INTEGRATION_ACTIVE=1

__cos_term_precmd() {{
    local ec=$?
    printf '\e]133;D;%d\a' "$ec"
    printf '\e]133;A\a'
}}
__cos_term_preexec() {{
    printf '\e]133;C\a'
}}
autoload -Uz add-zsh-hook
add-zsh-hook precmd __cos_term_precmd
add-zsh-hook preexec __cos_term_preexec

PS1="${{PS1}}%{{$(printf '\e]133;B\a')%}}"

setopt HIST_IGNORE_SPACE

__cos_ai() {{
    if [[ -z "$COS_AI_TMP" ]]; then
        echo "cosmic-term AI not active in this shell." >&2
        return 1
    fi
    local qfile="$COS_AI_TMP/aq-$1.txt"
    local cfile="$COS_AI_TMP/ac-$1.json"
    if [[ ! -f "$qfile" ]]; then
        echo "cos-ai: query file $qfile missing." >&2
        return 1
    fi
    local fl
    if IFS= read -r fl < "$qfile" && [[ -n "$fl" ]]; then
        local trunc="$fl"
        if (( ${{#trunc}} > 80 )); then trunc="${{trunc:0:80}}..."; fi
        printf '\e[A\r\e[2K\e[36m@ \e[39m%s\n' "$trunc"
    fi
    local prompt
    if [[ -f "$cfile" ]]; then
        prompt="<terminal_context>$(cat "$cfile")</terminal_context>

<user_request>
$(cat "$qfile")
</user_request>"
    else
        prompt="$(cat "$qfile")"
    fi
    {copilot_bin} -p "$prompt" {copilot_flags}
    local rc=$?
    rm -f "$qfile" "$cfile"
    return $rc
}}
"#
    )
}

// ────────────────────────── Fish ──────────────────────────

fn fish_integration(copilot_flags: &str, copilot_bin: &str) -> String {
    format!(
        r#"# cosmic-term AI integration — auto-generated, do not edit.
if test -n "$__COS_TERM_SHELL_INTEGRATION_ACTIVE"
    exit 0
end
set -gx __COS_TERM_SHELL_INTEGRATION_ACTIVE 1

function __cos_term_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end

function __cos_term_postexec --on-event fish_postexec
    printf '\e]133;D;%d\a' $status
    printf '\e]133;A\a'
end

function __cos_ai
    if test -z "$COS_AI_TMP"
        echo "cosmic-term AI not active in this shell." >&2
        return 1
    end
    set -l qfile "$COS_AI_TMP/aq-$argv[1].txt"
    set -l cfile "$COS_AI_TMP/ac-$argv[1].json"
    if not test -f "$qfile"
        echo "cos-ai: query file $qfile missing." >&2
        return 1
    end
    set -l fl (head -n1 "$qfile")
    set -l trunc "$fl"
    if test (string length -- "$trunc") -gt 80
        set trunc (string sub --length 80 -- "$trunc")"..."
    end
    printf '\e[A\r\e[2K\e[36m@ \e[39m%s\n' "$trunc"
    set -l prompt
    if test -f "$cfile"
        set prompt "<terminal_context>"(cat "$cfile")"</terminal_context>

<user_request>
"(cat "$qfile")"
</user_request>"
    else
        set prompt (cat "$qfile")
    end
    {copilot_bin} -p "$prompt" {copilot_flags}
    set -l rc $status
    rm -f "$qfile" "$cfile"
    return $rc
end
"#
    )
}

// ────────────────────────── PowerShell ──────────────────────────

fn pwsh_integration(copilot_flags: &str, copilot_bin: &str) -> String {
    format!(
        r##"# cosmic-term AI integration — auto-generated, do not edit.
if ($env:__COS_TERM_SHELL_INTEGRATION_ACTIVE -eq '1') {{ return }}
$env:__COS_TERM_SHELL_INTEGRATION_ACTIVE = '1'

# Inject OSC 133 markers via prompt function override.
$global:__cos_term_inner_prompt = $function:prompt
function global:prompt {{
    $ec = if ($LASTEXITCODE -ne $null) {{ $LASTEXITCODE }} else {{ 0 }}
    "$([char]27)]133;D;$ec$([char]7)$([char]27)]133;A$([char]7)" + (& $global:__cos_term_inner_prompt) + "$([char]27)]133;B$([char]7)"
}}

# Filter `__cos_ai` lines from PSReadLine history.
if (Get-Module -ListAvailable PSReadLine) {{
    Set-PSReadLineOption -AddToHistoryHandler {{
        param([string]$line)
        if ($line.TrimStart() -match '^__cos_ai') {{ return $false }}
        return $true
    }}
}}

function global:__cos_ai {{
    param([string]$id)
    if (-not $env:COS_AI_TMP) {{
        Write-Error "cosmic-term AI not active in this shell."
        return
    }}
    $qfile = Join-Path $env:COS_AI_TMP "aq-$id.txt"
    $cfile = Join-Path $env:COS_AI_TMP "ac-$id.json"
    if (-not (Test-Path $qfile)) {{
        Write-Error "cos-ai: query file $qfile missing."
        return
    }}
    $query = Get-Content -Raw $qfile
    $trunc = ($query -split "`n")[0]
    if ($trunc.Length -gt 80) {{ $trunc = $trunc.Substring(0, 80) + "..." }}
    Write-Host "`e[A`r`e[2K`e[36m@ `e[39m$trunc"
    $prompt = if (Test-Path $cfile) {{
        "<terminal_context>$(Get-Content -Raw $cfile)</terminal_context>`n`n<user_request>`n$query`n</user_request>"
    }} else {{ $query }}
    & {copilot_bin} -p $prompt {copilot_flags}
    $rc = $LASTEXITCODE
    Remove-Item -ErrorAction SilentlyContinue $qfile, $cfile
    return $rc
}}
"##
    )
}

// ────────────────────────── cmd.exe stub ──────────────────────────

const CMD_STUB: &str = r#"@echo off
echo cosmic-term AI is not supported in cmd.exe. Use bash / zsh / fish / pwsh.
exit /b 1
"#;
