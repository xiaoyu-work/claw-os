use serde_json::{json, Value};

/// `cos agent vision <subcommand>` — surface for the
/// [`crate::agent::media::vision::routing`] policy layer.
///
/// Currently only `route` is implemented: given an image
/// descriptor (size + mime + intent) and a policy (provider vision
/// support, OCR availability, native cap, vision-enabled toggle),
/// report the [`RoutingDecision`] (Native / Ocr / Skip + reason).
///
/// Two input modes:
///
/// * `--bytes N --mime <m>` — synthesise a descriptor without
///   reading any actual image. Useful for previewing decisions in
///   tests / scripts.
/// * `--file <path>` — read the file's size on disk; mime is
///   inferred from the extension unless `--mime` overrides it. The
///   file is **not** loaded into memory; only `metadata().len()` is
///   used.
///
/// Policy flags map 1:1 to [`RoutingPolicy`] fields. Defaults
/// match `RoutingPolicy::default()` (no provider vision, no OCR,
/// 5 MiB native cap, vision enabled).
pub(super) fn vision_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    match sub {
        "route" => vision_route_cmd(&args[1..]),
        "sniff" => vision_sniff_cmd(&args[1..]),
        "analyze" => vision_analyze_cmd(&args[1..]),
        "" => Err("usage: cos agent vision <route|sniff|analyze> ... \
             (e.g. route --file <p> | sniff --file <p> | analyze --file <p> --prompt <t>)"
            .to_string()),
        other => Err(format!(
            "unknown vision subcommand: {other}. try: route | sniff | analyze"
        )),
    }
}

/// `cos agent vision sniff --file <path> | --url <url>`
///
/// Read the head of an image (file or URL), report the magic-byte
/// MIME, the byte length, and whether it's a "widely-supported"
/// vision MIME. Pure inspection — does not call any LLM.
fn vision_sniff_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::media::vision::analyze::sniff_mime;
    use crate::agent::media::vision::routing::ImageMime;

    let mut file: Option<String> = None;
    let mut url: Option<String> = None;
    let mut head_only_bytes: usize = 32; // sniff_mime needs ~12 bytes max
    let mut fetch_timeout_ms: u64 = 30_000;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                file = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--file needs a path".to_string())?,
                );
                i += 2;
            }
            "--url" => {
                url = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--url needs a value".to_string())?,
                );
                i += 2;
            }
            "--head-bytes" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--head-bytes needs a number".to_string())?;
                head_only_bytes = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--head-bytes parse: {e}"))?;
                i += 2;
            }
            "--fetch-timeout-ms" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--fetch-timeout-ms needs a number".to_string())?;
                fetch_timeout_ms = raw
                    .parse::<u64>()
                    .map_err(|e| format!("--fetch-timeout-ms parse: {e}"))?;
                i += 2;
            }
            other => return Err(format!("unknown vision sniff flag: {other}")),
        }
    }

    if file.is_some() == url.is_some() {
        return Err("vision sniff needs exactly one of --file <path> or --url <url>".to_string());
    }

    let (bytes_len, head, source) = if let Some(path) = file {
        let p = std::path::PathBuf::from(&path);
        let meta = std::fs::metadata(&p).map_err(|e| format!("stat {path}: {e}"))?;
        let bytes_len = meta.len() as usize;
        let data = std::fs::read(&p).map_err(|e| format!("read {path}: {e}"))?;
        let head_n = head_only_bytes.min(data.len());
        (bytes_len, data[..head_n].to_vec(), format!("file:{path}"))
    } else {
        let u = url.unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime: {e}"))?;
        let (data, _mime) = runtime
            .block_on(crate::agent::media::vision::analyze::fetch_image(
                &u,
                std::time::Duration::from_millis(fetch_timeout_ms),
            ))
            .map_err(|e| format!("fetch {u}: {e}"))?;
        let head_n = head_only_bytes.min(data.len());
        (data.len(), data[..head_n].to_vec(), format!("url:{u}"))
    };

    let mime = sniff_mime(&head);
    Ok(json!({
        "source": source,
        "bytes_len": bytes_len,
        "head_bytes_inspected": head.len(),
        "mime": format!("{:?}", mime),
        "mime_widely_supported": mime.is_widely_supported(),
        "is_other": matches!(mime, ImageMime::Other),
    }))
}

/// `cos agent vision analyze --file <path> | --url <url> | --base64 <data> --mime <m>
///                           --prompt <text> [--system <text>] [--max-tokens N]
///                           [--provider <name>] [--model <name>]
///                           [--fetch-timeout-ms N]`
///
/// End-to-end vision call: resolves the image to base64, builds a
/// multimodal chat request, dispatches via the configured (or
/// overridden) provider, and prints the assistant's text response.
fn vision_analyze_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::media::vision::analyze::{analyze, ImageInput, VisionRequest};
    use crate::agent::media::vision::routing::ImageMime;

    let mut file: Option<String> = None;
    let mut url: Option<String> = None;
    let mut base64_data: Option<String> = None;
    let mut mime_override: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut system: Option<String> = None;
    let mut max_tokens: Option<u32> = None;
    let mut provider_override: Option<String> = None;
    let mut model_override: Option<String> = None;
    let mut fetch_timeout_ms: u64 = 30_000;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                file = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--file needs a path".to_string())?,
                );
                i += 2;
            }
            "--url" => {
                url = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--url needs a value".to_string())?,
                );
                i += 2;
            }
            "--base64" => {
                base64_data = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--base64 needs a value".to_string())?,
                );
                i += 2;
            }
            "--mime" => {
                mime_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--mime needs a value".to_string())?,
                );
                i += 2;
            }
            "--prompt" => {
                prompt = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--prompt needs a value".to_string())?,
                );
                i += 2;
            }
            "--system" => {
                system = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--system needs a value".to_string())?,
                );
                i += 2;
            }
            "--max-tokens" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--max-tokens needs a number".to_string())?;
                max_tokens = Some(
                    raw.parse::<u32>()
                        .map_err(|e| format!("--max-tokens parse: {e}"))?,
                );
                i += 2;
            }
            "--provider" => {
                provider_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--provider needs a value".to_string())?,
                );
                i += 2;
            }
            "--model" => {
                model_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--model needs a value".to_string())?,
                );
                i += 2;
            }
            "--fetch-timeout-ms" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--fetch-timeout-ms needs a number".to_string())?;
                fetch_timeout_ms = raw
                    .parse::<u64>()
                    .map_err(|e| format!("--fetch-timeout-ms parse: {e}"))?;
                i += 2;
            }
            other => return Err(format!("unknown vision analyze flag: {other}")),
        }
    }

    let prompt = prompt.ok_or_else(|| "vision analyze: --prompt <text> required".to_string())?;
    if prompt.trim().is_empty() {
        return Err("vision analyze: --prompt must be non-empty".to_string());
    }

    // Mutually-exclusive image source. base64 needs an explicit mime.
    let sources_set = usize::from(file.is_some())
        + usize::from(url.is_some())
        + usize::from(base64_data.is_some());
    if sources_set != 1 {
        return Err(
            "vision analyze needs exactly one of --file <path> | --url <url> | --base64 <data>"
                .to_string(),
        );
    }

    let image: ImageInput = if let Some(path) = file {
        let data = std::fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;
        // Honour --mime if supplied; otherwise infer from extension; sniff
        // bytes as last resort so HEIC/BMP etc still get classified.
        let mime = if let Some(m) = mime_override.as_deref() {
            ImageMime::from_str(m)
        } else {
            let ext = std::path::Path::new(&path)
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            let by_ext = ImageMime::from_str(&ext);
            if matches!(by_ext, ImageMime::Other) {
                crate::agent::media::vision::analyze::sniff_mime(&data)
            } else {
                by_ext
            }
        };
        ImageInput::Bytes { data, mime }
    } else if let Some(u) = url {
        if let Some(m) = mime_override.as_deref() {
            // Caller supplied mime → fetch eagerly so we can pass Bytes
            // (skips fetch_image's per-byte mime sniff, lets caller win).
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let (data, _mime) = runtime
                .block_on(crate::agent::media::vision::analyze::fetch_image(
                    &u,
                    std::time::Duration::from_millis(fetch_timeout_ms),
                ))
                .map_err(|e| format!("fetch {u}: {e}"))?;
            ImageInput::Bytes {
                data,
                mime: ImageMime::from_str(m),
            }
        } else {
            ImageInput::Url(u)
        }
    } else {
        let data = base64_data.unwrap();
        let mime = mime_override
            .as_deref()
            .ok_or_else(|| "--base64 requires --mime <m>".to_string())?;
        ImageInput::Base64 {
            data,
            mime: ImageMime::from_str(mime),
        }
    };

    let cfg = crate::config::get();
    let provider_name = provider_override
        .clone()
        .unwrap_or_else(|| cfg.agent.provider.clone());
    if provider_name.trim().is_empty() {
        return Err(
            "no provider configured (set agent.provider in config or pass --provider)".to_string(),
        );
    }
    let model_name = model_override
        .clone()
        .or_else(|| {
            if cfg.agent.model.is_empty() {
                None
            } else {
                Some(cfg.agent.model.clone())
            }
        })
        .ok_or_else(|| {
            "no model configured (set agent.model in config or pass --model)".to_string()
        })?;

    let provider = crate::agent::llm::registry::build(&provider_name, &model_name, &cfg.agent)
        .map_err(|e| format!("build provider {provider_name}: {e}"))?;
    let provider = crate::ai::gate::wrap_for_system(provider);

    let mut req = VisionRequest::new(prompt.clone(), image);
    req.system = system;
    req.max_tokens = max_tokens;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    let resp = runtime
        .block_on(analyze(
            provider.as_ref(),
            req,
            std::time::Duration::from_millis(fetch_timeout_ms),
        ))
        .map_err(|e| format!("vision analyze: {e}"))?;

    Ok(json!({
        "ok": true,
        "provider": provider_name,
        "model": model_name,
        "answer": resp.text,
        "model_reported": resp.model,
    }))
}

fn vision_route_cmd(args: &[String]) -> Result<Value, String> {
    use crate::agent::media::vision::routing::{
        route, ImageDescriptor, ImageIntent, ImageMime, RoutingDecision, RoutingPolicy,
    };

    let mut file: Option<String> = None;
    let mut bytes_override: Option<usize> = None;
    let mut mime_override: Option<String> = None;
    let mut intent = ImageIntent::General;
    let mut policy = RoutingPolicy::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--file" => {
                file = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--file needs a path".to_string())?,
                );
                i += 2;
            }
            "--bytes" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--bytes needs a number".to_string())?;
                bytes_override = Some(
                    raw.parse::<usize>()
                        .map_err(|e| format!("--bytes parse: {e}"))?,
                );
                i += 2;
            }
            "--mime" => {
                mime_override = Some(
                    args.get(i + 1)
                        .cloned()
                        .ok_or_else(|| "--mime needs a value".to_string())?,
                );
                i += 2;
            }
            "--intent" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--intent needs a value".to_string())?;
                intent = match raw.to_ascii_lowercase().as_str() {
                    "general" => ImageIntent::General,
                    "extract-text" | "extract_text" => ImageIntent::ExtractText,
                    "identify" => ImageIntent::Identify,
                    "caption" => ImageIntent::Caption,
                    other => {
                        return Err(format!(
                            "unknown --intent: {other}. try: general | extract-text | identify | caption"
                        ))
                    }
                };
                i += 2;
            }
            "--provider-vision" => {
                policy.provider_supports_vision = true;
                i += 1;
            }
            "--no-provider-vision" => {
                policy.provider_supports_vision = false;
                i += 1;
            }
            "--vision-disabled" => {
                policy.vision_enabled = false;
                i += 1;
            }
            "--vision-enabled" => {
                policy.vision_enabled = true;
                i += 1;
            }
            "--ocr-available" => {
                policy.ocr_available = true;
                i += 1;
            }
            "--no-ocr" => {
                policy.ocr_available = false;
                i += 1;
            }
            "--max-native-bytes" => {
                let raw = args
                    .get(i + 1)
                    .ok_or_else(|| "--max-native-bytes needs a number".to_string())?;
                policy.max_native_bytes = raw
                    .parse::<usize>()
                    .map_err(|e| format!("--max-native-bytes parse: {e}"))?;
                i += 2;
            }
            other => return Err(format!("unknown vision route flag: {other}")),
        }
    }

    let (bytes_len, mime, source) = match (file.as_ref(), bytes_override, mime_override.as_ref()) {
        (None, None, _) => {
            return Err(
                "vision route needs --file <path> or --bytes N (and --mime if no --file)"
                    .to_string(),
            );
        }
        (Some(path), _, _) => {
            let p = std::path::PathBuf::from(path);
            let meta = std::fs::metadata(&p).map_err(|e| format!("stat {path}: {e}"))?;
            // --bytes overrides the on-disk size if both supplied (rare;
            // useful when previewing what would happen if we shrank the file).
            let len = bytes_override.unwrap_or(meta.len() as usize);
            // If --mime was supplied, honour it. Otherwise infer from extension.
            let m = match mime_override.as_deref() {
                Some(mime_str) => ImageMime::from_str(mime_str),
                None => {
                    let ext = p
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|s| s.to_ascii_lowercase())
                        .unwrap_or_default();
                    ImageMime::from_str(&ext)
                }
            };
            (len, m, format!("file:{path}"))
        }
        (None, Some(b), Some(m)) => (b, ImageMime::from_str(m), "synthetic".to_string()),
        (None, Some(_), None) => {
            return Err("--bytes requires --mime when --file is not supplied".to_string());
        }
    };

    let descriptor = ImageDescriptor {
        bytes_len,
        mime,
        intent,
    };
    let decision = route(&descriptor, &policy);
    let (verdict, reason) = match decision {
        RoutingDecision::Native => ("native", None),
        RoutingDecision::Ocr => ("ocr", None),
        RoutingDecision::Skip { reason } => ("skip", Some(reason)),
    };

    Ok(json!({
        "source": source,
        "descriptor": {
            "bytes_len": descriptor.bytes_len,
            "mime": format!("{:?}", descriptor.mime),
            "mime_widely_supported": descriptor.mime.is_widely_supported(),
            "intent": format!("{:?}", descriptor.intent),
        },
        "policy": {
            "provider_supports_vision": policy.provider_supports_vision,
            "vision_enabled": policy.vision_enabled,
            "max_native_bytes": policy.max_native_bytes,
            "ocr_available": policy.ocr_available,
        },
        "decision": verdict,
        "reason": reason,
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/vision_commands.rs"
    ));
}
