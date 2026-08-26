// SPDX-License-Identifier: GPL-3.0-only

use serde::Deserialize;
use std::{
    ffi::OsString,
    io,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ITEMS: usize = 20;
const PREVIEW_CHARS: usize = 180;

const SNAPSHOT_SCRIPT: &str = r#"
var clipboardTab = str(config('clipboard_tab'));
if (!clipboardTab) {
    print(JSON.stringify({error: 'no-clipboard-tab'}));
    abort();
}
tab(clipboardTab);
var entries = [];
var previousTextIdentity = '';
var itemCount = size();
for (var row = 0; row < itemCount && entries.length < 20; ++row) {
    var item = getItem(row);
    if (!item || !(mimeText in item)) {
        continue;
    }
    var data = item[mimeText];
    var text = str(data);
    if (!text || text.indexOf('\0') !== -1) {
        continue;
    }
    var textIdentity = str(toBase64(sha256sum(data)));
    if (textIdentity === previousTextIdentity) {
        continue;
    }
    previousTextIdentity = textIdentity;
    var entry = {
        identity: str(toBase64(sha256sum(pack(item)))),
        text: text
    };
    var copyTimeMime = 'application/x-copyq-user-copy-time';
    if (copyTimeMime in item) {
        entry.copied_at = str(item[copyTimeMime]);
    }
    entries.push(entry);
}
print(JSON.stringify(entries));
"#;

const RESTORE_SCRIPT: &str = r#"
var wantedIdentity = str(input());
var clipboardTab = str(config('clipboard_tab'));
if (!clipboardTab) {
    print('no-clipboard-tab');
    abort();
}
tab(clipboardTab);
var found = false;
var itemCount = size();
for (var currentRow = 0; currentRow < itemCount; ++currentRow) {
    var item = getItem(currentRow);
    if (item && (mimeText in item)) {
        var identity = str(toBase64(sha256sum(pack(item))));
        if (identity === wantedIdentity) {
            select(currentRow);
            found = true;
            break;
        }
    }
}
print(found ? 'ok' : 'not-found');
"#;

const REMOVE_SCRIPT: &str = r#"
var wantedIdentity = str(input());
var clipboardTab = str(config('clipboard_tab'));
if (!clipboardTab) {
    print('no-clipboard-tab');
    abort();
}
tab(clipboardTab);
var found = false;
var itemCount = size();
for (var currentRow = 0; currentRow < itemCount; ++currentRow) {
    var item = getItem(currentRow);
    if (item && (mimeText in item)) {
        var identity = str(toBase64(sha256sum(pack(item))));
        if (identity === wantedIdentity) {
            remove(currentRow);
            found = true;
            break;
        }
    }
}
print(found ? 'ok' : 'not-found');
"#;

const CLEAR_SCRIPT: &str = r#"
var clipboardTab = str(config('clipboard_tab'));
if (!clipboardTab) {
    print('no-clipboard-tab');
} else {
    tab(clipboardTab);
    var remaining = size();
    while (remaining > 0) {
        var first = Math.max(0, remaining - 100);
        for (var row = remaining - 1; row >= first; --row) {
            remove(row);
        }
        remaining = size();
    }
    print('ok');
}
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardEntry {
    pub identity: String,
    pub preview: String,
    pub copied_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawSnapshotEntry {
    identity: String,
    text: String,
    #[serde(default)]
    copied_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SnapshotOutput {
    Entries(Vec<RawSnapshotEntry>),
    Error { error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopyqCommand {
    Snapshot,
    Restore,
    Remove,
    Clear,
}

impl CopyqCommand {
    fn script(self) -> &'static str {
        match self {
            Self::Snapshot => SNAPSHOT_SCRIPT,
            Self::Restore => RESTORE_SCRIPT,
            Self::Remove => REMOVE_SCRIPT,
            Self::Clear => CLEAR_SCRIPT,
        }
    }

    fn args(self) -> Vec<OsString> {
        vec!["eval".into(), self.script().into()]
    }
}

pub async fn load_history() -> Result<Vec<ClipboardEntry>, String> {
    let output = run(CopyqCommand::Snapshot, None).await?;
    parse_snapshot(&output)
}

pub async fn restore(identity: String) -> Result<(), String> {
    run_identity_action(CopyqCommand::Restore, &identity).await
}

pub async fn remove(identity: String) -> Result<(), String> {
    run_identity_action(CopyqCommand::Remove, &identity).await
}

pub async fn clear() -> Result<(), String> {
    let output = run(CopyqCommand::Clear, None).await?;
    match String::from_utf8_lossy(&output).trim() {
        "ok" => Ok(()),
        "no-clipboard-tab" => {
            Err("CopyQ does not have a configured clipboard history tab.".to_string())
        }
        _ => Err("CopyQ returned an invalid clear result.".to_string()),
    }
}

async fn run_identity_action(command: CopyqCommand, identity: &str) -> Result<(), String> {
    let output = run(command, Some(identity.as_bytes())).await?;
    match String::from_utf8_lossy(&output).trim() {
        "ok" => Ok(()),
        "not-found" => Err(
            "The clipboard item changed or disappeared before the action completed.".to_string(),
        ),
        "no-clipboard-tab" => {
            Err("CopyQ does not have a configured clipboard history tab.".to_string())
        }
        _ => Err("CopyQ returned an invalid action result.".to_string()),
    }
}

async fn run(command: CopyqCommand, input: Option<&[u8]>) -> Result<Vec<u8>, String> {
    let operation = async {
        let mut child = Command::new(copyq_binary());
        child
            .args(command.args())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = child.spawn()?;
        if let Some(input) = input {
            let mut stdin = child.stdin.take().ok_or_else(|| {
                io::Error::new(io::ErrorKind::BrokenPipe, "CopyQ stdin was unavailable")
            })?;
            stdin.write_all(input).await?;
            stdin.shutdown().await?;
        }
        child.wait_with_output().await
    };

    let output: Output = timeout(COMMAND_TIMEOUT, operation)
        .await
        .map_err(|_| "CopyQ did not respond in time.".to_string())?
        .map_err(map_start_error)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err("CopyQ is not running or could not complete the request.".to_string())
    }
}

fn parse_snapshot(raw: &[u8]) -> Result<Vec<ClipboardEntry>, String> {
    let raw_entries = match serde_json::from_slice::<SnapshotOutput>(raw)
        .map_err(|_| "CopyQ returned an invalid clipboard history snapshot.".to_string())?
    {
        SnapshotOutput::Entries(entries) => entries,
        SnapshotOutput::Error { error } if error == "no-clipboard-tab" => {
            return Err("CopyQ does not have a configured clipboard history tab.".to_string());
        }
        SnapshotOutput::Error { .. } => {
            return Err("CopyQ returned an invalid clipboard history snapshot.".to_string());
        }
    };
    let mut entries = Vec::new();
    let mut previous_identity: Option<String> = None;

    for raw in raw_entries {
        if raw.identity.is_empty()
            || raw.text.is_empty()
            || raw.text.contains('\0')
            || previous_identity.as_deref() == Some(raw.identity.as_str())
        {
            continue;
        }
        previous_identity = Some(raw.identity.clone());
        entries.push(ClipboardEntry {
            identity: raw.identity,
            preview: preview(&raw.text, PREVIEW_CHARS),
            copied_at: raw
                .copied_at
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        });
        if entries.len() == MAX_ITEMS {
            break;
        }
    }
    Ok(entries)
}

fn map_start_error(error: io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        "CopyQ is not installed.".to_string()
    } else {
        "CopyQ is not running or could not be reached.".to_string()
    }
}

fn copyq_binary() -> OsString {
    std::env::var_os("COPYQ_BIN").unwrap_or_else(|| "copyq".into())
}

fn preview(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/copyq.rs"
    ));
}
