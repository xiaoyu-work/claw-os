use serde_json::{json, Value};

/// `cos agent media <providers|outputs-dir|list-outputs [--limit N] [--ext <e>]>`
///
/// Surfaces the media subsystem so operators can introspect:
///   * which TTS / STT / image-gen providers are wired up and which
///     are configured (currently only the `noop` reference impls
///     are auto-registered;  cloud factories will populate this
///     surface once `with_*_providers_from_cfg` lands)
///   * where rendered audio / image artifacts are written
///   * what's recently been generated under that directory
pub(super) fn media_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("providers");
    match sub {
        "providers" | "" => {
            let cfg = crate::config::current_snapshot();
            let tts = crate::agent::media::factory::tts_registry_from_cfg(&cfg);
            let stt = crate::agent::media::factory::stt_registry_from_cfg(&cfg);
            let imagegen = crate::agent::media::factory::imagegen_registry_from_cfg(&cfg);

            let tts_rows: Vec<_> = tts
                .names()
                .into_iter()
                .map(|name| {
                    let configured =
                        tts.get(&name).map(|p| p.is_configured()).unwrap_or(false);
                    json!({"name": name, "configured": configured})
                })
                .collect();
            let stt_rows: Vec<_> = stt
                .names()
                .into_iter()
                .map(|name| {
                    let configured =
                        stt.get(&name).map(|p| p.is_configured()).unwrap_or(false);
                    json!({"name": name, "configured": configured})
                })
                .collect();
            let imagegen_rows: Vec<_> = imagegen
                .names()
                .into_iter()
                .map(|name| {
                    let configured = imagegen
                        .get(&name)
                        .map(|p| p.is_configured())
                        .unwrap_or(false);
                    json!({"name": name, "configured": configured})
                })
                .collect();

            Ok(json!({
                "outputs_dir": crate::paths::agent_media_outputs_dir().display().to_string(),
                "tts": {
                    "n": tts_rows.len(),
                    "providers": tts_rows,
                },
                "stt": {
                    "n": stt_rows.len(),
                    "providers": stt_rows,
                },
                "imagegen": {
                    "n": imagegen_rows.len(),
                    "providers": imagegen_rows,
                },
            }))
        }
        "outputs-dir" => Ok(json!({
            "path": crate::paths::agent_media_outputs_dir().display().to_string(),
        })),
        "list-outputs" => {
            let mut limit: usize = 20;
            let mut ext_filter: Option<String> = None;
            let mut i = 1usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--limit" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--limit needs <n>".to_string())?;
                        limit = v
                            .parse()
                            .map_err(|_| format!("--limit must be a positive integer, got: {v}"))?;
                        i += 2;
                    }
                    "--ext" => {
                        let v = args
                            .get(i + 1)
                            .ok_or_else(|| "--ext needs <extension>".to_string())?;
                        ext_filter =
                            Some(v.trim_start_matches('.').to_ascii_lowercase());
                        i += 2;
                    }
                    other => {
                        return Err(format!(
                            "unknown flag for `media list-outputs`: {other}"
                        ));
                    }
                }
            }
            let dir = crate::paths::agent_media_outputs_dir();
            list_media_outputs(&dir, limit, ext_filter.as_deref())
        }
        "play" => media_play_cmd(&args[1..]),
        "playback-status" => media_playback_status_cmd(&args[1..]),
        other => Err(format!(
            "unknown media subcommand: {other}. try: providers | outputs-dir | list-outputs [--limit N] [--ext <e>] | play <path> | playback-status [--format wav|mp3|ogg|flac]"
        )),
    }
}

fn list_media_outputs(
    dir: &std::path::Path,
    limit: usize,
    ext_filter: Option<&str>,
) -> Result<Value, String> {
    if !dir.exists() {
        return Ok(json!({
            "dir": dir.display().to_string(),
            "exists": false,
            "limit": limit,
            "n": 0,
            "files": Vec::<Value>::new(),
        }));
    }
    let mut rows: Vec<(std::time::SystemTime, std::path::PathBuf, u64, String)> = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir failed: {e}"))?;
    for ent in entries.flatten() {
        let path = ent.path();
        let meta = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if let Some(want) = ext_filter {
            if ext != want {
                continue;
            }
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        rows.push((mtime, path, meta.len(), ext));
    }
    // Newest first.
    rows.sort_by_key(|row| std::cmp::Reverse(row.0));
    rows.truncate(limit);
    let files: Vec<Value> = rows
        .into_iter()
        .map(|(mtime, path, size, ext)| {
            let mtime_ms = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            json!({
                "path": path.display().to_string(),
                "name": path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string(),
                "ext": ext,
                "size": size,
                "mtime_ms": mtime_ms,
            })
        })
        .collect();
    Ok(json!({
        "dir": dir.display().to_string(),
        "exists": true,
        "limit": limit,
        "ext_filter": ext_filter,
        "n": files.len(),
        "files": files,
    }))
}

// =====================================================================
// `cos agent media play <path>` — short-term blocking playback via
// the OS's native audio facility (PlaySoundW on Windows, afplay on
// macOS, format-aware CLI player on Linux). See
// `crate::agent::media::voice::system_playback` for the semantic
// contract and what's intentionally out of scope.
// =====================================================================

fn media_play_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::media::voice::system_playback;
    use std::path::PathBuf;

    let mut path: Option<PathBuf> = None;
    let mut detect_only = false;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--detect" => {
                detect_only = true;
                i += 1;
            }
            "--" => {
                if let Some(p) = args.get(i + 1) {
                    path = Some(PathBuf::from(p));
                }
                i += 2;
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown flag for `media play`: {other}"));
            }
            _ => {
                if path.is_some() {
                    return Err(format!(
                        "unexpected extra argument to `media play`: {}",
                        args[i]
                    ));
                }
                path = Some(PathBuf::from(&args[i]));
                i += 1;
            }
        }
    }

    let path = path.ok_or("usage: cos agent media play <path> [--detect]")?;

    // Format detection up front so we always report it, even on error.
    let format = system_playback::PlaybackFormat::from_path(&path);
    let format_str = format.map(|f| f.as_str().to_string());

    if detect_only {
        let player = format.and_then(system_playback::detect_player);
        return Ok(json!({
            "path": path.display().to_string(),
            "format": format_str,
            "player": player,
            "playable": player.is_some(),
        }));
    }

    match system_playback::play_file_blocking(&path) {
        Ok(()) => Ok(json!({
            "ok": true,
            "path": path.display().to_string(),
            "format": format_str,
        })),
        Err(e) => Err(format!("playback failed: {e}")),
    }
}

fn media_playback_status_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::media::voice::system_playback::{detect_player, PlaybackFormat};

    let mut filter: Option<PlaybackFormat> = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--format" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--format needs <wav|mp3|ogg|flac>".to_string())?;
                filter = Some(match v.to_ascii_lowercase().as_str() {
                    "wav" => PlaybackFormat::Wav,
                    "mp3" => PlaybackFormat::Mp3,
                    "ogg" | "oga" => PlaybackFormat::Ogg,
                    "flac" => PlaybackFormat::Flac,
                    other => {
                        return Err(format!(
                            "--format: unknown value '{other}'. try: wav | mp3 | ogg | flac"
                        ));
                    }
                });
                i += 2;
            }
            other => return Err(format!("unknown flag for `media playback-status`: {other}")),
        }
    }

    let formats: Vec<PlaybackFormat> = match filter {
        Some(f) => vec![f],
        None => vec![
            PlaybackFormat::Wav,
            PlaybackFormat::Mp3,
            PlaybackFormat::Ogg,
            PlaybackFormat::Flac,
        ],
    };

    let rows: Vec<Value> = formats
        .iter()
        .map(|f| {
            let player = detect_player(*f);
            json!({
                "format": f.as_str(),
                "player": player,
                "playable": player.is_some(),
            })
        })
        .collect();

    Ok(json!({
        "os": std::env::consts::OS,
        "formats": rows,
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/media_commands.rs"
    ));
}
