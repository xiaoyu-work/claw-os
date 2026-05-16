//! cos-cli — interactive REPL / TUI front-end for the cos kernel
//! agent (Phase 7 scaffold).
//!
//! Today this binary forwards subcommands to the `cos` binary via
//! a child process. Phase 7 will replace that with an in-process
//! handle once the agent runtime exposes an embeddable API surface
//! (issue tracked under p7-cli-subcommands).
//!
//! Usage shape (all aspirational; today only `ask` works, and only
//! by re-shelling out to `cos agent ask`):
//!
//! ```text
//! cos-cli                          # enter REPL
//! cos-cli ask "what's the time?"   # one-shot
//! cos-cli skill list               # delegate to cos agent skill list
//! ```

use std::io::{self, BufRead, Write};
use std::process::Command;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "cos-cli",
    version,
    about = "Interactive shell for the cos agent (Phase 7 scaffold)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<CliCmd>,

    /// Override the path to the `cos` binary (default: `cos`).
    #[arg(long, env = "COS_BIN", default_value = "cos")]
    cos_bin: String,
}

#[derive(Debug, Subcommand)]
enum CliCmd {
    /// One-shot ask — equivalent to `cos agent ask "<prompt>"`.
    Ask {
        /// Joined into one prompt string.
        #[arg(num_args = 1..)]
        prompt: Vec<String>,
    },
    /// Print agent status — equivalent to `cos agent status`.
    Status,
    /// Forward arbitrary args to `cos agent ...`.
    Agent {
        #[arg(num_args = 1.., trailing_var_arg = true)]
        args: Vec<String>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        None => repl(&cli.cos_bin),
        Some(CliCmd::Ask { prompt }) => one_shot_ask(&cli.cos_bin, prompt),
        Some(CliCmd::Status) => forward(&cli.cos_bin, &["agent", "status"]),
        Some(CliCmd::Agent { args }) => {
            let mut cmd: Vec<&str> = vec!["agent"];
            cmd.extend(args.iter().map(String::as_str));
            forward(&cli.cos_bin, &cmd)
        }
    }
}

fn one_shot_ask(cos_bin: &str, prompt: Vec<String>) -> Result<()> {
    let prompt = prompt.join(" ");
    if prompt.trim().is_empty() {
        return Err(anyhow!("ask requires a non-empty prompt"));
    }
    // Pass `--` so prompts that start with a dash (e.g. `--help` or
    // `-foo`) are not interpreted as flags by `cos agent ask`. Without
    // the separator, `cos cli ask -- --version` and similar inputs
    // would be parsed as version requests instead of prompts.
    forward(cos_bin, &["agent", "ask", "--", &prompt])
}

fn forward(cos_bin: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cos_bin)
        .args(args)
        .status()
        .map_err(|e| anyhow!("failed to spawn `{cos_bin}`: {e}"))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// Minimal REPL: read a line, treat it as the prompt, forward to
/// `cos agent ask`. `exit` / `quit` / EOF exit cleanly. Slash
/// commands (`/status`, `/help`) trigger the matching subcommand.
fn repl(cos_bin: &str) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    println!("cos-cli {} — type /help for commands, EOF or /quit to exit", env!("CARGO_PKG_VERSION"));
    loop {
        write!(stdout, "cos> ")?;
        stdout.flush()?;
        let mut line = String::new();
        let n = stdin.lock().read_line(&mut line)?;
        if n == 0 {
            println!();
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed {
            "/quit" | "/exit" | "exit" | "quit" => return Ok(()),
            "/help" => {
                println!("commands:");
                println!("  /status   — show agent status");
                println!("  /quit     — exit");
                println!("  <text>    — send as prompt to `cos agent ask`");
            }
            "/status" => {
                let _ = forward(cos_bin, &["agent", "status"]);
            }
            other if other.starts_with('/') => {
                eprintln!("unknown slash command: {other}");
            }
            prompt => {
                // Same `--` separator as `one_shot_ask`: keep
                // REPL-typed prompts that happen to start with `-`
                // from being parsed as flags by `cos agent ask`.
                let _ = forward(cos_bin, &["agent", "ask", "--", prompt]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cli_parses_ask_subcommand() {
        let cli = Cli::try_parse_from(["cos-cli", "ask", "hello", "world"]).unwrap();
        match cli.cmd {
            Some(CliCmd::Ask { prompt }) => assert_eq!(prompt, vec!["hello", "world"]),
            _ => panic!("expected Ask"),
        }
    }

    #[test]
    fn cli_parses_status_subcommand() {
        let cli = Cli::try_parse_from(["cos-cli", "status"]).unwrap();
        assert!(matches!(cli.cmd, Some(CliCmd::Status)));
    }

    #[test]
    fn cli_parses_agent_passthrough() {
        let cli = Cli::try_parse_from(["cos-cli", "agent", "model", "list"]).unwrap();
        match cli.cmd {
            Some(CliCmd::Agent { args }) => assert_eq!(args, vec!["model", "list"]),
            _ => panic!("expected Agent"),
        }
    }

    #[test]
    fn cli_defaults_to_repl_when_no_subcommand() {
        let cli = Cli::try_parse_from(["cos-cli"]).unwrap();
        assert!(cli.cmd.is_none());
    }

    #[test]
    fn cos_bin_env_default() {
        let cli = Cli::try_parse_from(["cos-cli"]).unwrap();
        // Default value when env var unset.
        assert_eq!(cli.cos_bin, "cos");
    }

    #[test]
    fn one_shot_ask_rejects_empty_prompt() {
        let err = one_shot_ask("cos", vec!["   ".into()]).unwrap_err();
        assert!(err.to_string().contains("non-empty"));
    }
}
