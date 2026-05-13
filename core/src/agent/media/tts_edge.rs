//! Microsoft Edge TTS provider — free, no-API-key, WebSocket-based.
//!
//! Edge TTS is the same backend that powers Microsoft Edge's "Read
//! Aloud" feature. Microsoft does not (publicly) charge for it and it
//! does not require a subscription key — only a stable client token
//! `6A5AA1D4EAFF4E9FB37E23D68491D6F4` that has been hardcoded in every
//! open-source Edge TTS client for years (`rany2/edge-tts`,
//! `BoyHagemann/edge-tts`, etc.). It is *not* a documented or
//! supported public API; we treat it as best-effort and surface
//! protocol drift as `MediaError::Provider`.
//!
//! ## Protocol
//!
//! Endpoint:
//! `wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?TrustedClientToken=...`
//!
//! After the WebSocket handshake the client sends two text frames and
//! then reads binary audio frames followed by a `Path:turn.end`
//! marker:
//!
//! 1. **Speech config** (text frame) — declares the desired output
//!    format. The body is a single-line JSON.
//! 2. **SSML** (text frame) — the actual text to synthesize, wrapped
//!    in `<speak>...<voice>...<prosody>...` tags.
//! 3. **Server frames** — interleaved text frames (`Path:turn.start`,
//!    `Path:audio.metadata`, `Path:turn.end`) and binary frames
//!    (`Path:audio` carrying the actual audio bytes).
//!
//! Each binary frame begins with a 2-byte big-endian header length,
//! followed by ASCII headers in MIME-ish format, followed by the
//! audio payload. We slice the payload off and concatenate.
//!
//! `turn.end` with no audio bytes accumulated is treated as an error
//! (most often: SSML rejected, voice not recognized, or rate limit).
//!
//! ## Output formats
//!
//! Edge TTS exposes a fixed menu of output formats. We expose four to
//! callers via [`AudioFormat`]:
//!
//! | requested            | edge `outputFormat`                | `sample_rate` |
//! |----------------------|------------------------------------|---------------|
//! | `Mp3` *(default)*    | `audio-24khz-48kbitrate-mono-mp3`  | 24_000        |
//! | `Wav`                | `riff-24khz-16bit-mono-pcm`        | 24_000        |
//! | `Ogg`                | `ogg-24khz-16bit-mono-opus`        | 24_000        |
//! | `Pcm16`              | `raw-24khz-16bit-mono-pcm`         | 24_000        |
//!
//! `AudioFormat::Other` requests are rejected (no silent format
//! coercion — callers explicit about what they want should get what
//! they asked for or an error).
//!
//! ## Configuration
//!
//! [`EdgeTtsConfig`]:
//!   * `default_voice`     — falls back to `en-US-AriaNeural`.
//!   * `base_url`          — override the WSS endpoint (rare; used
//!                           only when Microsoft retires the token).
//!   * `extra_headers`     — pass-through headers; provider-required
//!                           headers (`Origin`, `User-Agent`) win.
//!   * `request_timeout`   — wraps the whole synthesize call;
//!                           `Duration::ZERO` disables.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use super::tts::{AudioFormat, TtsProvider, TtsRequest, TtsResponse};
use super::MediaError;

const PROVIDER_NAME: &str = "edge-tts";

/// Public Edge "Read Aloud" endpoint. The trusted client token is the
/// same value every open-source Edge TTS client uses; it has been
/// stable for years. Override via `base_url` if it ever rotates.
const DEFAULT_ENDPOINT: &str = "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?TrustedClientToken=6A5AA1D4EAFF4E9FB37E23D68491D6F4";

/// Trusted client token baked into Microsoft Edge's "Read Aloud"
/// browser feature. Used as the salt for the Sec-MS-GEC token.
const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";

/// Pinned Chromium build number reported in `Sec-MS-GEC-Version`. Must
/// match a real published Chromium / Edge build that the upstream
/// server recognizes; stale values get 403'd.
const CHROMIUM_FULL_VERSION: &str = "143.0.3650.75";
const CHROMIUM_MAJOR_VERSION: &str = "143";

const DEFAULT_VOICE: &str = "en-US-AriaNeural";

/// User-Agent matching the recent Microsoft Edge build pinned above.
/// Edge TTS rejects (403) UAs that don't match the live channel.
const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0";

/// `Origin` header used by Edge's "Read Aloud" extension. Required —
/// without it the server returns 403.
const DEFAULT_ORIGIN: &str = "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold";

/// Seconds between the Windows NT epoch (1601-01-01 UTC) and the Unix
/// epoch (1970-01-01 UTC). Used by [`generate_sec_ms_gec`] to convert
/// a Unix timestamp into Windows file time format.
const WIN_EPOCH_SECONDS: u64 = 11_644_473_600;

/// Hard cap on the synthesized audio buffer — protects against a
/// runaway server response with no `turn.end`.
const MAX_AUDIO_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct EdgeTtsConfig {
    pub default_voice: Option<String>,
    pub base_url: String,
    pub extra_headers: HashMap<String, String>,
    pub request_timeout: Duration,
}

impl Default for EdgeTtsConfig {
    fn default() -> Self {
        Self {
            default_voice: None,
            base_url: DEFAULT_ENDPOINT.to_string(),
            extra_headers: HashMap::new(),
            request_timeout: Duration::from_secs(30),
        }
    }
}

pub struct EdgeTtsProvider {
    cfg: EdgeTtsConfig,
}

impl EdgeTtsProvider {
    pub fn new(cfg: EdgeTtsConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl TtsProvider for EdgeTtsProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn is_configured(&self) -> bool {
        // Edge TTS is keyless. Configuration is always "complete".
        true
    }

    async fn synthesize(&self, request: TtsRequest) -> Result<TtsResponse, MediaError> {
        request.validate()?;
        let format = request.format.unwrap_or(AudioFormat::Mp3);
        let (output_format, sample_rate) = output_format_for(format)?;

        let voice = request
            .voice
            .clone()
            .or_else(|| self.cfg.default_voice.clone())
            .unwrap_or_else(|| DEFAULT_VOICE.to_string());
        let rate = format_rate(request.speed);

        let request_id = new_request_id();
        let connection_id = new_request_id();
        let now_unix = current_unix_seconds();
        let now_js = utc_now_js_style(now_unix);
        let sec_ms_gec = generate_sec_ms_gec(now_unix);
        let sec_ms_gec_version = format!("1-{CHROMIUM_FULL_VERSION}");
        let url = build_endpoint_url(
            &self.cfg.base_url,
            &connection_id,
            &sec_ms_gec,
            &sec_ms_gec_version,
        )?;
        let config_frame = build_config_frame(&now_js, output_format);
        let ssml_frame = build_ssml_frame(&now_js, &request_id, &voice, &rate, &request.text);

        let fut = run_synthesize(&self.cfg, &url, &config_frame, &ssml_frame);
        let audio = if self.cfg.request_timeout.is_zero() {
            fut.await?
        } else {
            tokio::time::timeout(self.cfg.request_timeout, fut)
                .await
                .map_err(|_| MediaError::Transport("edge tts: request timed out".to_string()))??
        };

        if audio.is_empty() {
            return Err(MediaError::Provider {
                status: 200,
                message: "edge tts: turn.end with no audio (likely invalid voice or SSML)"
                    .to_string(),
            });
        }

        Ok(TtsResponse {
            audio,
            format,
            sample_rate: Some(sample_rate),
        })
    }
}

/// Map an [`AudioFormat`] to the matching Edge TTS `outputFormat`
/// string and a sample rate. Edge supports a fixed menu of formats,
/// all 24 kHz mono — we expose the four that have a clean
/// [`AudioFormat`] mapping.
fn output_format_for(fmt: AudioFormat) -> Result<(&'static str, u32), MediaError> {
    Ok(match fmt {
        AudioFormat::Mp3 => ("audio-24khz-48kbitrate-mono-mp3", 24_000),
        AudioFormat::Wav => ("riff-24khz-16bit-mono-pcm", 24_000),
        AudioFormat::Ogg => ("ogg-24khz-16bit-mono-opus", 24_000),
        AudioFormat::Pcm16 => ("raw-24khz-16bit-mono-pcm", 24_000),
        AudioFormat::Other => {
            return Err(MediaError::InvalidRequest(
                "edge tts: AudioFormat::Other not supported — request mp3/wav/ogg/pcm16"
                    .to_string(),
            ));
        }
    })
}

/// Generate a 32-character lowercase hex request ID. Edge TTS is
/// strict about this — UUIDs with hyphens get rejected.
fn new_request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Current Unix epoch in seconds (with sub-second precision discarded
/// for stability — the GEC token is rounded to 5-minute buckets so
/// fractional seconds don't matter).
fn current_unix_seconds() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Render the JS-style timestamp Edge's "Read Aloud" extension uses
/// for `X-Timestamp`:
/// `Mon Jan 02 2006 15:04:05 GMT+0000 (Coordinated Universal Time)`.
/// Mismatched timestamp shapes used to work, but recent server builds
/// 400 anything that doesn't look JS-shaped.
fn utc_now_js_style(unix_secs: u64) -> String {
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(unix_secs as i64, 0)
        .unwrap_or_else(chrono::Utc::now);
    dt.format("%a %b %d %Y %H:%M:%S GMT+0000 (Coordinated Universal Time)")
        .to_string()
}

/// Generate the `Sec-MS-GEC` token. Mirrors `rany2/edge-tts`:
///
/// 1. Take the current Unix timestamp.
/// 2. Shift to Windows file time epoch (1601-01-01).
/// 3. Round down to a 5-minute bucket.
/// 4. Convert to 100-nanosecond Windows file ticks.
/// 5. SHA-256 of `format!("{ticks}{TRUSTED_CLIENT_TOKEN}")`, uppercase
///    hex.
///
/// See: https://github.com/rany2/edge-tts/issues/290#issuecomment-2464956570
fn generate_sec_ms_gec(unix_secs: u64) -> String {
    // ticks (in seconds, in Windows file time epoch), bucketed to 5
    // minutes.
    let mut ticks_seconds = unix_secs.saturating_add(WIN_EPOCH_SECONDS);
    ticks_seconds -= ticks_seconds % 300;
    // Windows file time = 100-nanosecond intervals, i.e. seconds *
    // 10_000_000.
    let ticks: u128 = (ticks_seconds as u128).saturating_mul(10_000_000);
    let to_hash = format!("{ticks}{TRUSTED_CLIENT_TOKEN}");
    crate::crypto::sha256_hex(to_hash.as_bytes()).to_ascii_uppercase()
}

/// Append `ConnectionId`, `Sec-MS-GEC`, `Sec-MS-GEC-Version` query
/// params to the configured base URL. The base URL is expected to
/// already carry `?TrustedClientToken=...`; we just `&`-append. If the
/// caller has overridden it without a query string we fall back to
/// `?`-prefixing.
fn build_endpoint_url(
    base: &str,
    connection_id: &str,
    sec_ms_gec: &str,
    sec_ms_gec_version: &str,
) -> Result<String, MediaError> {
    if base.is_empty() {
        return Err(MediaError::InvalidRequest(
            "edge tts: base_url is empty".to_string(),
        ));
    }
    let sep = if base.contains('?') { '&' } else { '?' };
    Ok(format!(
        "{base}{sep}ConnectionId={connection_id}&Sec-MS-GEC={sec_ms_gec}&Sec-MS-GEC-Version={sec_ms_gec_version}"
    ))
}

/// Format the SSML `<prosody rate=...>` value for a given speed. Edge
/// expects a percent string with explicit sign — `+0%`, `+50%`, `-25%`.
/// `None` and `1.0` both render as `+0%`.
fn format_rate(speed: Option<f32>) -> String {
    let s = speed.unwrap_or(1.0);
    let pct = ((s - 1.0) * 100.0).round() as i32;
    if pct >= 0 {
        format!("+{pct}%")
    } else {
        format!("{pct}%")
    }
}

/// Escape SSML/XML special characters in the input text. Edge will
/// 400 the request if `<` or `&` appears unescaped.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn build_config_frame(now: &str, output_format: &str) -> String {
    format!(
        "X-Timestamp:{now}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n{{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"false\"}},\"outputFormat\":\"{output_format}\"}}}}}}}}"
    )
}

fn build_ssml_frame(now: &str, request_id: &str, voice: &str, rate: &str, text: &str) -> String {
    let escaped_text = xml_escape(text);
    let escaped_voice = xml_escape(voice);
    let body = format!(
        "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'><voice name='{escaped_voice}'><prosody pitch='+0Hz' rate='{rate}' volume='+0%'>{escaped_text}</prosody></voice></speak>"
    );
    // The trailing `Z` on `X-Timestamp` is a documented Edge server
    // quirk — see rany2/edge-tts `ssml_headers_plus_data`. The
    // speech.config frame does NOT get this `Z`.
    format!(
        "X-RequestId:{request_id}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{now}Z\r\nPath:ssml\r\n\r\n{body}"
    )
}

async fn run_synthesize(
    cfg: &EdgeTtsConfig,
    url: &str,
    config_frame: &str,
    ssml_frame: &str,
) -> Result<Vec<u8>, MediaError> {
    let mut req = url
        .into_client_request()
        .map_err(|e| MediaError::InvalidRequest(format!("edge tts: bad base_url: {e}")))?;

    {
        let headers = req.headers_mut();
        // Apply caller-provided extras first so provider-required
        // headers below win on conflict.
        for (k, v) in &cfg.extra_headers {
            // Forbid anything that would corrupt the WS handshake.
            let lower = k.to_ascii_lowercase();
            if lower.starts_with("sec-websocket-")
                || lower == "upgrade"
                || lower == "connection"
                || lower == "host"
            {
                continue;
            }
            if let (Ok(name), Ok(val)) = (
                tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                headers.insert(name, val);
            }
        }
        // Required:
        headers.insert("Origin", HeaderValue::from_static(DEFAULT_ORIGIN));
        headers.insert("User-Agent", HeaderValue::from_static(DEFAULT_USER_AGENT));
        // Browser-shaped headers (from Edge's own "Read Aloud"
        // extension). Without these recent server builds 403.
        headers.insert("Pragma", HeaderValue::from_static("no-cache"));
        headers.insert("Cache-Control", HeaderValue::from_static("no-cache"));
        headers.insert(
            "Accept-Encoding",
            HeaderValue::from_static("gzip, deflate, br, zstd"),
        );
        headers.insert(
            "Accept-Language",
            HeaderValue::from_static("en-US,en;q=0.9"),
        );
        // Sec-CH-UA hints help us blend in with a real Edge
        // handshake. Strict UA-based abuse heuristics expect them.
        let sec_ch_ua = format!(
            "\" Not;A Brand\";v=\"99\", \"Microsoft Edge\";v=\"{CHROMIUM_MAJOR_VERSION}\", \"Chromium\";v=\"{CHROMIUM_MAJOR_VERSION}\""
        );
        if let Ok(v) = HeaderValue::from_str(&sec_ch_ua) {
            headers.insert("Sec-CH-UA", v);
        }
        headers.insert("Sec-CH-UA-Mobile", HeaderValue::from_static("?0"));
        headers.insert(
            "Sec-CH-UA-Platform",
            HeaderValue::from_static("\"Windows\""),
        );
    }

    let (mut ws, _resp) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| MediaError::Transport(format!("edge tts: ws connect failed: {e}")))?;

    ws.send(Message::Text(config_frame.to_string().into()))
        .await
        .map_err(|e| MediaError::Transport(format!("edge tts: send config: {e}")))?;
    ws.send(Message::Text(ssml_frame.to_string().into()))
        .await
        .map_err(|e| MediaError::Transport(format!("edge tts: send ssml: {e}")))?;

    let mut audio: Vec<u8> = Vec::new();
    while let Some(msg) = ws.next().await {
        let msg = msg.map_err(|e| MediaError::Transport(format!("edge tts: ws recv: {e}")))?;
        match msg {
            Message::Text(text) => {
                let path = parse_text_path(&text);
                match path.as_deref() {
                    Some("turn.end") => break,
                    Some("turn.start") | Some("audio.metadata") | Some("response") => {
                        // Informational; loop continues. Errors come via
                        // either a close frame or an empty audio buffer
                        // at turn.end (handled by caller).
                    }
                    _ => {
                        // Unknown — ignore. Edge has added paths in the
                        // past (e.g. `audio.metadata` was newer than
                        // `turn.end`); we don't want to fail on benign
                        // additions.
                    }
                }
            }
            Message::Binary(data) => {
                let chunk = parse_binary_audio(&data)?;
                if !chunk.is_empty() {
                    if audio.len() + chunk.len() > MAX_AUDIO_BYTES {
                        return Err(MediaError::Provider {
                            status: 200,
                            message: format!(
                                "edge tts: response exceeded {} bytes",
                                MAX_AUDIO_BYTES
                            ),
                        });
                    }
                    audio.extend_from_slice(chunk);
                }
            }
            Message::Close(frame) => {
                if audio.is_empty() {
                    let reason = frame
                        .as_ref()
                        .map(|f| format!("{}: {}", u16::from(f.code), f.reason))
                        .unwrap_or_else(|| "no close frame".to_string());
                    return Err(MediaError::Provider {
                        status: 0,
                        message: format!("edge tts: server closed before audio ({reason})"),
                    });
                }
                break;
            }
            // Ignore Ping/Pong/Frame; tungstenite handles ping/pong
            // automatically when given a chance to flush.
            _ => {}
        }
    }

    let _ = ws.close(None).await;
    Ok(audio)
}

/// Pull the `Path:` header value out of an Edge text frame. Returns
/// `None` if the frame has no header section or no `Path:` line.
fn parse_text_path(frame: &str) -> Option<String> {
    // Headers are CRLF-separated; body (if any) follows a blank line.
    let header_block = frame.split("\r\n\r\n").next().unwrap_or(frame);
    for line in header_block.split("\r\n") {
        if let Some(rest) = line.strip_prefix("Path:") {
            return Some(rest.trim().to_string());
        }
        // The `X-RequestId` header is sometimes before `Path:`; we
        // skip non-matching lines.
    }
    None
}

/// Parse an Edge binary audio frame:
/// `[2 bytes BE header_len] [ASCII headers] [audio bytes]`. Returns a
/// slice over the audio tail. If the headers indicate a non-`audio`
/// path, returns an empty slice (caller skips it). Surfaces malformed
/// frames as `MediaError::Parse`.
fn parse_binary_audio(data: &[u8]) -> Result<&[u8], MediaError> {
    if data.len() < 2 {
        return Err(MediaError::Parse(
            "edge tts: binary frame shorter than 2-byte header length".to_string(),
        ));
    }
    let header_len = u16::from_be_bytes([data[0], data[1]]) as usize;
    let header_end = 2usize.saturating_add(header_len);
    if header_end > data.len() {
        return Err(MediaError::Parse(format!(
            "edge tts: binary frame header_len={} exceeds frame length {}",
            header_len,
            data.len()
        )));
    }
    let headers = std::str::from_utf8(&data[2..header_end])
        .map_err(|e| MediaError::Parse(format!("edge tts: binary frame headers not utf-8: {e}")))?;
    let mut path: Option<&str> = None;
    for line in headers.split("\r\n") {
        if let Some(rest) = line.strip_prefix("Path:") {
            path = Some(rest.trim());
            break;
        }
    }
    if path == Some("audio") {
        Ok(&data[header_end..])
    } else {
        Ok(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_edge_tts() {
        let p = EdgeTtsProvider::new(EdgeTtsConfig::default());
        assert_eq!(p.name(), "edge-tts");
    }

    #[test]
    fn always_configured_no_api_key_required() {
        let p = EdgeTtsProvider::new(EdgeTtsConfig::default());
        assert!(p.is_configured());
    }

    #[test]
    fn xml_escape_replaces_specials() {
        assert_eq!(xml_escape("a < b"), "a &lt; b");
        assert_eq!(xml_escape("a > b"), "a &gt; b");
        assert_eq!(xml_escape("a & b"), "a &amp; b");
        assert_eq!(xml_escape("'q'"), "&apos;q&apos;");
        assert_eq!(xml_escape("\"q\""), "&quot;q&quot;");
        assert_eq!(xml_escape("safe text"), "safe text");
    }

    #[test]
    fn xml_escape_handles_combo() {
        assert_eq!(
            xml_escape("<x>'a'&\"b\"</x>"),
            "&lt;x&gt;&apos;a&apos;&amp;&quot;b&quot;&lt;/x&gt;"
        );
    }

    #[test]
    fn format_rate_zero_default() {
        assert_eq!(format_rate(None), "+0%");
        assert_eq!(format_rate(Some(1.0)), "+0%");
    }

    #[test]
    fn format_rate_positive_speed() {
        assert_eq!(format_rate(Some(1.5)), "+50%");
        assert_eq!(format_rate(Some(2.0)), "+100%");
    }

    #[test]
    fn format_rate_negative_speed() {
        assert_eq!(format_rate(Some(0.5)), "-50%");
        assert_eq!(format_rate(Some(0.75)), "-25%");
    }

    #[test]
    fn output_format_known_formats() {
        let (f, sr) = output_format_for(AudioFormat::Mp3).unwrap();
        assert_eq!(f, "audio-24khz-48kbitrate-mono-mp3");
        assert_eq!(sr, 24_000);
        let (f, sr) = output_format_for(AudioFormat::Wav).unwrap();
        assert_eq!(f, "riff-24khz-16bit-mono-pcm");
        assert_eq!(sr, 24_000);
        let (f, sr) = output_format_for(AudioFormat::Ogg).unwrap();
        assert_eq!(f, "ogg-24khz-16bit-mono-opus");
        assert_eq!(sr, 24_000);
        let (f, sr) = output_format_for(AudioFormat::Pcm16).unwrap();
        assert_eq!(f, "raw-24khz-16bit-mono-pcm");
        assert_eq!(sr, 24_000);
    }

    #[test]
    fn output_format_other_rejected() {
        let err = output_format_for(AudioFormat::Other).unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[test]
    fn request_id_is_32_lowercase_hex() {
        let id = new_request_id();
        assert_eq!(id.len(), 32);
        assert!(id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn config_frame_includes_format_and_path() {
        let f = build_config_frame(
            "Wed, 06 May 2026 19:00:00 GMT",
            "audio-24khz-48kbitrate-mono-mp3",
        );
        assert!(f.contains("Path:speech.config"));
        assert!(f.contains("X-Timestamp:Wed, 06 May 2026 19:00:00 GMT"));
        assert!(f.contains("Content-Type:application/json"));
        assert!(f.contains("\"outputFormat\":\"audio-24khz-48kbitrate-mono-mp3\""));
        // Body separated from headers by a blank CRLF line.
        assert!(f.contains("\r\n\r\n"));
    }

    #[test]
    fn ssml_frame_has_namespace_and_voice() {
        let f = build_ssml_frame(
            "Wed, 06 May 2026 19:00:00 GMT",
            "abc123",
            "en-US-AriaNeural",
            "+0%",
            "Hello world",
        );
        assert!(f.contains("Path:ssml"));
        assert!(f.contains("X-RequestId:abc123"));
        assert!(f.contains("xmlns='http://www.w3.org/2001/10/synthesis'"));
        assert!(f.contains("name='en-US-AriaNeural'"));
        assert!(f.contains("rate='+0%'"));
        assert!(f.contains(">Hello world<"));
    }

    #[test]
    fn ssml_frame_escapes_text() {
        let f = build_ssml_frame("T", "id", "v", "+0%", "Tom & Jerry <chase>");
        assert!(f.contains("Tom &amp; Jerry &lt;chase&gt;"));
        // Raw `<chase>` must not appear in the body.
        assert!(!f.contains(">Tom & Jerry <chase><"));
    }

    #[test]
    fn parse_text_path_finds_path_line() {
        let frame = "X-RequestId:abc\r\nPath:turn.end\r\n\r\n";
        assert_eq!(parse_text_path(frame).as_deref(), Some("turn.end"));
    }

    #[test]
    fn parse_text_path_no_path_returns_none() {
        let frame = "X-RequestId:abc\r\n\r\n";
        assert_eq!(parse_text_path(frame), None);
    }

    #[test]
    fn parse_binary_audio_extracts_payload_after_audio_path() {
        let headers = b"X-RequestId:abc\r\nPath:audio\r\n\r\n";
        let mut frame = (headers.len() as u16).to_be_bytes().to_vec();
        frame.extend_from_slice(headers);
        let audio = b"\xff\xfb\x90\x00ID3";
        frame.extend_from_slice(audio);
        let payload = parse_binary_audio(&frame).unwrap();
        assert_eq!(payload, audio);
    }

    #[test]
    fn parse_binary_audio_returns_empty_for_non_audio_path() {
        let headers = b"X-RequestId:abc\r\nPath:audio.metadata\r\n\r\n";
        let mut frame = (headers.len() as u16).to_be_bytes().to_vec();
        frame.extend_from_slice(headers);
        frame.extend_from_slice(&[0xde, 0xad]);
        let payload = parse_binary_audio(&frame).unwrap();
        assert!(payload.is_empty());
    }

    #[test]
    fn parse_binary_audio_rejects_short_frame() {
        let err = parse_binary_audio(&[0u8]).unwrap_err();
        assert!(matches!(err, MediaError::Parse(_)));
    }

    #[test]
    fn parse_binary_audio_rejects_header_overflow() {
        // header_len=0xFFFF but frame is only 5 bytes total.
        let frame = vec![0xff, 0xff, 1, 2, 3];
        let err = parse_binary_audio(&frame).unwrap_err();
        match err {
            MediaError::Parse(msg) => assert!(msg.contains("exceeds frame length")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_binary_audio_rejects_non_utf8_headers() {
        // header_len=4, headers contain invalid UTF-8.
        let frame = vec![0, 4, 0xff, 0xff, 0xff, 0xff, b'a'];
        let err = parse_binary_audio(&frame).unwrap_err();
        match err {
            MediaError::Parse(msg) => assert!(msg.contains("not utf-8")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn synthesize_rejects_empty_text() {
        let p = EdgeTtsProvider::new(EdgeTtsConfig::default());
        let err = p.synthesize(TtsRequest::new("")).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn synthesize_rejects_audio_format_other() {
        let p = EdgeTtsProvider::new(EdgeTtsConfig::default());
        let mut r = TtsRequest::new("hi");
        r.format = Some(AudioFormat::Other);
        let err = p.synthesize(r).await.unwrap_err();
        assert!(matches!(err, MediaError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn synthesize_bad_base_url_is_invalid_request() {
        let mut cfg = EdgeTtsConfig::default();
        cfg.base_url = "http://[::1]:1?bad uri".to_string();
        let p = EdgeTtsProvider::new(cfg);
        let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
        match err {
            MediaError::InvalidRequest(msg) => assert!(msg.contains("bad base_url")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn synthesize_unreachable_base_url_is_transport_error() {
        let mut cfg = EdgeTtsConfig::default();
        // Routable but unreachable; tungstenite returns a transport
        // error rather than InvalidRequest.
        cfg.base_url = "ws://127.0.0.1:1/edge".to_string();
        cfg.request_timeout = Duration::from_millis(500);
        let p = EdgeTtsProvider::new(cfg);
        let err = p.synthesize(TtsRequest::new("hi")).await.unwrap_err();
        match err {
            MediaError::Transport(_) => {}
            other => panic!("expected Transport, got: {other:?}"),
        }
    }

    #[test]
    fn sec_ms_gec_is_64_uppercase_hex() {
        // Pin a known timestamp to make this deterministic.
        let token = generate_sec_ms_gec(1_700_000_000);
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(token.chars().all(|c| !c.is_lowercase()));
    }

    #[test]
    fn sec_ms_gec_is_5min_bucketed() {
        // Two timestamps inside the same 5-minute bucket → identical
        // tokens. (1_700_000_000 is at second 1_700_000_000 % 300 ==
        // 200 within its bucket, so adding 50 stays inside.)
        let a = generate_sec_ms_gec(1_700_000_000);
        let b = generate_sec_ms_gec(1_700_000_050);
        assert_eq!(a, b);
        // Crossing into the next bucket changes the token.
        let c = generate_sec_ms_gec(1_700_000_300);
        assert_ne!(a, c);
    }

    #[test]
    fn build_endpoint_url_appends_with_ampersand_when_base_has_query() {
        let url = build_endpoint_url("wss://x?foo=bar", "abc", "DEAD", "1-1.0").unwrap();
        assert_eq!(
            url,
            "wss://x?foo=bar&ConnectionId=abc&Sec-MS-GEC=DEAD&Sec-MS-GEC-Version=1-1.0"
        );
    }

    #[test]
    fn build_endpoint_url_appends_with_question_mark_when_base_has_no_query() {
        let url = build_endpoint_url("wss://x/edge/v1", "abc", "DEAD", "1-1.0").unwrap();
        assert_eq!(
            url,
            "wss://x/edge/v1?ConnectionId=abc&Sec-MS-GEC=DEAD&Sec-MS-GEC-Version=1-1.0"
        );
    }

    #[test]
    fn build_endpoint_url_rejects_empty_base() {
        let err = build_endpoint_url("", "a", "b", "c").unwrap_err();
        match err {
            MediaError::InvalidRequest(msg) => assert!(msg.contains("base_url is empty")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn ssml_frame_x_timestamp_has_trailing_z() {
        // Edge server quirk — the SSML frame's X-Timestamp gets an
        // extra `Z` on the end. The speech.config frame does NOT.
        let f = build_ssml_frame(
            "Mon Jan 02 2006 15:04:05 GMT+0000 (UTC)",
            "rid",
            "v",
            "+0%",
            "x",
        );
        assert!(f.contains("X-Timestamp:Mon Jan 02 2006 15:04:05 GMT+0000 (UTC)Z\r\n"));
    }

    #[test]
    fn config_frame_x_timestamp_has_no_trailing_z() {
        let f = build_config_frame(
            "Mon Jan 02 2006 15:04:05 GMT+0000 (UTC)",
            "audio-24khz-48kbitrate-mono-mp3",
        );
        assert!(f.contains("X-Timestamp:Mon Jan 02 2006 15:04:05 GMT+0000 (UTC)\r\n"));
        assert!(!f.contains("UTC)Z"));
    }

    #[test]
    fn js_style_timestamp_format() {
        // Pinned: 2023-11-14 22:13:20 UTC.
        let s = utc_now_js_style(1_700_000_000);
        // Sanity checks — we don't pin the exact day-of-week to avoid
        // tz library drift, but the suffix must be exact.
        assert!(s.ends_with(" GMT+0000 (Coordinated Universal Time)"));
        assert!(s.contains("2023"));
        assert!(s.contains("Nov"));
    }
}
