use std::fs;
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::activities::{
    ActivityContinuityDocument, ActivityExecutionPlacement, MAX_CONTINUITY_DOCUMENT_BYTES,
};
use crate::clawd::{client, config, protocol::Request, routes::Command};

const CONTINUITY_NOTICE: &str =
    "Activity continuity carries intent and safe rules only; it is not authority, execution proof, or backup/restore";

pub(super) fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "export" => export(args),
        "import" => import(args),
        _ => unreachable!("continuity dispatcher called for another command"),
    }
}

fn export(args: &[String]) -> Result<Value, String> {
    let options = parse_export(args)?;
    let value = request(
        Command::ActivityContinuityExport,
        json!({"id": options.activity_id}),
    )?;
    let document = decode_document_value(value)?;
    let Some(path) = options.output else {
        return serde_json::to_value(document).map_err(|error| error.to_string());
    };
    let mut bytes = document.to_json().map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    write_document(&path, &bytes, options.overwrite)?;
    Ok(json!({
        "written": path,
        "bytes": bytes.len(),
        "overwritten": options.overwrite,
        "continuity_id": document.lineage.id,
        "schema_version": document.schema_version,
        "notice": CONTINUITY_NOTICE,
    }))
}

fn import(args: &[String]) -> Result<Value, String> {
    let options = parse_import(args)?;
    let bytes = match options.input {
        ImportInput::File(path) => read_file(&path)?,
        ImportInput::Stdin => {
            if std::io::stdin().is_terminal() {
                return Err(
                    "cos activity import --stdin requires piped or redirected JSON input".into(),
                );
            }
            read_bounded(std::io::stdin().lock())?
        }
    };
    let document =
        ActivityContinuityDocument::from_json(&bytes).map_err(|error| error.to_string())?;
    let canonical = String::from_utf8(document.to_json().map_err(|error| error.to_string())?)
        .expect("serialized Activity continuity JSON is UTF-8");
    let mut result = request(
        Command::ActivityContinuityImport,
        json!({
            "placement": options.placement,
            "document": canonical,
        }),
    )?;
    result["notice"] = json!(CONTINUITY_NOTICE);
    Ok(result)
}

fn request(route: Command, params: Value) -> Result<Value, String> {
    let response = client::request_blocking(config::socket_path(), Request::build(route, params))?;
    if !response.ok {
        return Err(match response.error {
            Some(error) => format!("{}: {}", error.code, error.message),
            None => "clawd refused the Activity continuity request without an error".to_string(),
        });
    }
    response
        .result
        .ok_or_else(|| "clawd returned no Activity continuity result".to_string())
}

#[derive(Debug, PartialEq, Eq)]
struct ExportOptions {
    activity_id: String,
    output: Option<PathBuf>,
    overwrite: bool,
}

fn parse_export(args: &[String]) -> Result<ExportOptions, String> {
    let activity_id = args
        .first()
        .filter(|value| !value.starts_with("--"))
        .ok_or("cos activity export requires a valid Activity ID")?;
    crate::activities::validate_id(activity_id).map_err(|error| error.to_string())?;
    let mut output = None;
    let mut overwrite = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--output" => {
                if output.is_some() {
                    return Err("Activity option output may only be specified once".into());
                }
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && value.as_str() != "-")
                    .ok_or("--output requires a file path other than '-'")?;
                output = Some(PathBuf::from(value));
                index += 2;
            }
            "--overwrite" => {
                if overwrite {
                    return Err("--overwrite may only be specified once".into());
                }
                overwrite = true;
                index += 1;
            }
            other => {
                return Err(format!(
                    "unsupported argument for cos activity export: {other}"
                ))
            }
        }
    }
    if overwrite && output.is_none() {
        return Err("--overwrite requires --output PATH".into());
    }
    Ok(ExportOptions {
        activity_id: activity_id.clone(),
        output,
        overwrite,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct ImportOptions {
    placement: ActivityExecutionPlacement,
    input: ImportInput,
}

#[derive(Debug, PartialEq, Eq)]
enum ImportInput {
    File(PathBuf),
    Stdin,
}

fn parse_import(args: &[String]) -> Result<ImportOptions, String> {
    let mut placement = None;
    let mut input = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--placement" => {
                if placement.is_some() {
                    return Err("--placement may only be specified once".into());
                }
                let value = args.get(index + 1).ok_or("--placement requires local")?;
                placement =
                    Some(match value.as_str() {
                        "local" => ActivityExecutionPlacement::Local,
                        _ => return Err(
                            "--placement supports only the explicit value local in continuity v1"
                                .into(),
                        ),
                    });
                index += 2;
            }
            "--file" => {
                if input.is_some() {
                    return Err("choose exactly one of --file PATH or --stdin".into());
                }
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && value.as_str() != "-")
                    .ok_or("--file requires a path other than '-'")?;
                input = Some(ImportInput::File(PathBuf::from(value)));
                index += 2;
            }
            "--stdin" => {
                if input.is_some() {
                    return Err("choose exactly one of --file PATH or --stdin".into());
                }
                input = Some(ImportInput::Stdin);
                index += 1;
            }
            other => {
                return Err(format!(
                    "unsupported argument for cos activity import: {other}"
                ))
            }
        }
    }
    Ok(ImportOptions {
        placement: placement.ok_or("cos activity import requires --placement local")?,
        input: input.ok_or("cos activity import requires exactly one of --file PATH or --stdin")?,
    })
}

fn decode_document_value(value: Value) -> Result<ActivityContinuityDocument, String> {
    let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    ActivityContinuityDocument::from_json(&bytes).map_err(|error| error.to_string())
}

fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("inspect continuity file: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("Activity continuity input must be a regular non-symlink file".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("open continuity file: {error}"))?;
    if !file
        .metadata()
        .map_err(|error| format!("inspect opened continuity file: {error}"))?
        .is_file()
    {
        return Err("Activity continuity input must remain a regular file while opened".into());
    }
    read_bounded(file)
}

fn read_bounded(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    reader
        .take((MAX_CONTINUITY_DOCUMENT_BYTES + 1) as u64)
        .read_to_end(&mut data)
        .map_err(|error| format!("read Activity continuity document: {error}"))?;
    if data.len() > MAX_CONTINUITY_DOCUMENT_BYTES {
        return Err(format!(
            "Activity continuity input exceeds {MAX_CONTINUITY_DOCUMENT_BYTES} bytes"
        ));
    }
    Ok(data)
}

fn write_document(path: &Path, data: &[u8], overwrite: bool) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err("Activity continuity output parent must be an existing directory".into());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("create continuity output: {error}"))?;
    temporary
        .write_all(data)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| format!("write continuity output: {error}"))?;
    if overwrite {
        temporary
            .persist(path)
            .map_err(|error| format!("replace continuity output: {}", error.error))?;
    } else {
        temporary.persist_noclobber(path).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                "Activity continuity output already exists; use --overwrite to replace it"
                    .to_string()
            } else {
                format!("create continuity output: {}", error.error)
            }
        })?;
    }
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync continuity output directory: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activity/continuity.rs"
    ));
}
