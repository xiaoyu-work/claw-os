//! Token-based auth for `cos agent serve`.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::agent::web::state::AppState;

pub fn token_dir() -> PathBuf {
    crate::paths::data_dir().join("agent").join("web")
}

pub fn token_path() -> PathBuf {
    token_dir().join("serve.token")
}

pub fn load_or_generate_token() -> Result<String, String> {
    if let Ok(data) = fs::read_to_string(token_path()) {
        let trimmed = data.trim();
        if trimmed.len() >= 16 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(trimmed.to_string());
        }
    }
    let mut bytes = [0u8; 32];
    fill_random(&mut bytes);
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    persist_token(&hex)
}

pub fn persist_token(hex: &str) -> Result<String, String> {
    let dir = token_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let path = token_path();
    let mut f = fs::File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(hex.as_bytes())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    drop(f);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(hex.to_string())
}

fn fill_random(buf: &mut [u8]) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::process::id().hash(&mut h);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut h);
    (buf.as_ptr() as usize).hash(&mut h);
    let mut seed = h.finish();
    for byte in buf.iter_mut() {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        *byte = (z & 0xff) as u8;
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub async fn require_token(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let public = matches!(path.as_str(), "/" | "/index.html" | "/favicon.ico");
    if public {
        return next.run(req).await;
    }

    let qs = req.uri().query().unwrap_or("");
    let token_from_query = parse_token_from_query(qs);
    let token_from_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_string);

    let presented = token_from_query.or(token_from_header).unwrap_or_default();
    let expected = state.inner.token.clone();

    if presented.is_empty() || !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"unauthorized","hint":"append ?t=<token> or set Authorization: Bearer <token>"}"#,
        )
            .into_response();
    }
    next.run(req).await
}

fn parse_token_from_query(qs: &str) -> Option<String> {
    for pair in qs.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        let v = it.next().unwrap_or("");
        if k == "t" || k == "token" {
            return Some(percent_decode(v));
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
