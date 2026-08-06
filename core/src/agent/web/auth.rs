//! Token-based auth for `cos agent serve`.

use std::fs;
use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::agent::web::state::AppState;

const DEFAULT_ACCESS_TTL_SECS: u64 = 60 * 60;
const MAX_ACCESS_TTL_SECS: u64 = 60 * 60;
const CLOCK_SKEW_SECS: u64 = 60;
type HmacSha256 = Hmac<Sha256>;

enum SecretLoad {
    Missing,
    Valid(String),
    Invalid(String),
}

pub fn token_dir() -> PathBuf {
    crate::paths::data_dir().join("agent").join("web")
}

pub fn token_path() -> PathBuf {
    token_dir().join("serve.token")
}

pub fn signing_key_path() -> PathBuf {
    token_dir().join("serve.signing-key")
}

pub fn load_or_generate_token() -> Result<String, String> {
    match load_hex_secret(&token_path())? {
        SecretLoad::Valid(secret) => return Ok(secret),
        SecretLoad::Invalid(reason) => {
            tracing::warn!(%reason, "regenerating invalid web bootstrap secret");
        }
        SecretLoad::Missing => {}
    }
    let hex = generate_secret("web bootstrap token")?;
    persist_token(&hex)
}

pub fn persist_token(hex: &str) -> Result<String, String> {
    validate_secret(hex)?;
    persist_secret(&token_path(), hex)?;
    Ok(hex.to_string())
}

fn load_bootstrap_token() -> Result<String, String> {
    match load_hex_secret(&token_path())? {
        SecretLoad::Valid(secret) => Ok(secret),
        SecretLoad::Missing => Err("web bootstrap token is missing; restart the server".to_string()),
        SecretLoad::Invalid(reason) => Err(reason),
    }
}

pub fn rotate_tokens() -> Result<String, String> {
    let bootstrap = generate_secret("web bootstrap token")?;
    let signing_key = generate_secret("web signing key")?;
    persist_secret(&signing_key_path(), &signing_key)?;
    if let Err(error) = persist_secret(&token_path(), &bootstrap) {
        return Err(format!(
            "signing key rotated and existing access tokens are invalid, but bootstrap rotation failed: {error}"
        ));
    }
    Ok(bootstrap)
}

pub fn ensure_signing_key() -> Result<(), String> {
    let path = signing_key_path();
    let secret = match load_hex_secret(&path)? {
        SecretLoad::Valid(secret) => secret,
        SecretLoad::Missing => {
            let secret = generate_secret("web signing key")?;
            persist_secret(&path, &secret)?;
            secret
        }
        SecretLoad::Invalid(reason) => {
            tracing::warn!(%reason, "regenerating invalid web signing key");
            let secret = generate_secret("web signing key")?;
            persist_secret(&path, &secret)?;
            secret
        }
    };
    let _ = hex::decode(secret).map_err(|error| format!("decode web signing key: {error}"))?;
    Ok(())
}

fn load_signing_key() -> Result<Vec<u8>, String> {
    match load_hex_secret(&signing_key_path())? {
        SecretLoad::Valid(secret) => {
            hex::decode(secret).map_err(|error| format!("decode web signing key: {error}"))
        }
        SecretLoad::Missing => Err("web signing key is missing; restart the server".to_string()),
        SecretLoad::Invalid(reason) => Err(reason),
    }
}

fn load_hex_secret(path: &std::path::Path) -> Result<SecretLoad, String> {
    match fs::read_to_string(path) {
        Ok(data) => {
            let secret = data.trim();
            match validate_secret(secret) {
                Ok(()) => Ok(SecretLoad::Valid(secret.to_string())),
                Err(error) => Ok(SecretLoad::Invalid(format!(
                    "{} is invalid: {error}",
                    path.display()
                ))),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SecretLoad::Missing),
        Err(error) => Err(format!("read {}: {error}", path.display())),
    }
}

fn validate_secret(secret: &str) -> Result<(), String> {
    if secret.len() == 64 && secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("web bootstrap/signing secrets must be exactly 64 hex characters".to_string())
    }
}

fn generate_secret(label: &str) -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    crate::credential::os_random_bytes(&mut bytes)
        .map_err(|error| format!("generate {label}: {error}"))?;
    Ok(hex::encode(bytes))
}

fn persist_secret(path: &std::path::Path, secret: &str) -> Result<(), String> {
    validate_secret(secret)?;
    crate::storage::ensure_private_dir(&token_dir())
        .map_err(|error| format!("secure {}: {error}", token_dir().display()))?;
    crate::agent::util::atomic_write_with_fsync(path, secret.as_bytes())
        .map_err(|error| format!("write {}: {error}", path.display()))
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AccessClaims {
    version: u8,
    uid: u32,
    issued_at: u64,
    expires_at: u64,
    token_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssuedAccessToken {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_at: u64,
    pub expires_in: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum TokenExchangeError {
    #[error("invalid bootstrap token")]
    InvalidBootstrap,
    #[error("{0}")]
    Internal(String),
}

#[derive(Debug, Clone)]
pub struct AuthenticatedToken {
    pub uid: u32,
    pub token_id: String,
    pub expires_at: u64,
}

pub fn exchange_bootstrap_token(
    presented_bootstrap: &str,
    owner_uid: u32,
    ttl_seconds: Option<u64>,
) -> Result<IssuedAccessToken, TokenExchangeError> {
    let expected = load_bootstrap_token().map_err(TokenExchangeError::Internal)?;
    if !constant_time_eq(
        presented_bootstrap.as_bytes(),
        expected.as_bytes(),
    ) {
        return Err(TokenExchangeError::InvalidBootstrap);
    }
    let ttl = ttl_seconds
        .unwrap_or(DEFAULT_ACCESS_TTL_SECS)
        .clamp(60, MAX_ACCESS_TTL_SECS);
    let now = now_seconds();
    let claims = AccessClaims {
        version: 1,
        uid: owner_uid,
        issued_at: now,
        expires_at: now.saturating_add(ttl),
        token_id: generate_token_id().map_err(TokenExchangeError::Internal)?,
    };
    let payload = serde_json::to_vec(&claims)
        .map_err(|error| TokenExchangeError::Internal(format!("serialize access token: {error}")))?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
    let signed = format!("v1.{encoded}");
    let key = load_signing_key().map_err(TokenExchangeError::Internal)?;
    let mut mac = HmacSha256::new_from_slice(&key)
        .map_err(|_| TokenExchangeError::Internal("initialize access-token signer".to_string()))?;
    mac.update(signed.as_bytes());
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(mac.finalize().into_bytes());
    Ok(IssuedAccessToken {
        access_token: format!("{signed}.{signature}"),
        token_type: "Bearer",
        expires_at: claims.expires_at,
        expires_in: ttl,
    })
}

fn verify_access_token(token: &str, owner_uid: u32) -> Result<AuthenticatedToken, String> {
    if token.len() > 4096 {
        return Err("access token is too large".to_string());
    }
    let mut parts = token.split('.');
    let version = parts.next().unwrap_or_default();
    let payload = parts.next().unwrap_or_default();
    let signature = parts.next().unwrap_or_default();
    if version != "v1" || payload.is_empty() || signature.is_empty() || parts.next().is_some() {
        return Err("malformed access token".to_string());
    }
    let signed = format!("{version}.{payload}");
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| "malformed access-token signature".to_string())?;
    let key = load_signing_key()?;
    let mut mac = HmacSha256::new_from_slice(&key)
        .map_err(|_| "initialize access-token verifier".to_string())?;
    mac.update(signed.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| "invalid access-token signature".to_string())?;

    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| "malformed access-token payload".to_string())?;
    let claims: AccessClaims = serde_json::from_slice(&payload)
        .map_err(|_| "malformed access-token claims".to_string())?;
    let now = now_seconds();
    if claims.version != 1
        || claims.uid != owner_uid
        || claims.token_id.len() != 32
        || !claims
            .token_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || claims.expires_at <= now
        || claims.issued_at > now.saturating_add(CLOCK_SKEW_SECS)
        || claims.expires_at <= claims.issued_at
        || claims.expires_at.saturating_sub(claims.issued_at) > MAX_ACCESS_TTL_SECS
    {
        return Err("expired or invalid access-token claims".to_string());
    }
    Ok(AuthenticatedToken {
        uid: claims.uid,
        token_id: claims.token_id,
        expires_at: claims.expires_at,
    })
}

fn generate_token_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    crate::credential::os_random_bytes(&mut bytes)
        .map_err(|error| format!("generate access-token id: {error}"))?;
    Ok(hex::encode(bytes))
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub async fn require_token(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let public = matches!(
        path.as_str(),
        "/" | "/index.html"
            | "/favicon.ico"
            | "/favicon.png"
            | "/clawos-symbol.png"
            | "/clawos-symbol-dark.png"
            | "/apple-touch-icon.png"
    ) || path.starts_with("/assets/")
        || path == "/api/auth/token";
    if public {
        return next.run(req).await;
    }

    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or_default();
    let authenticated = match verify_access_token(presented, state.inner.owner_uid) {
        Ok(authenticated) => authenticated,
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"error":"unauthorized","hint":"exchange the bootstrap secret at POST /api/auth/token and use the returned Bearer token"}"#,
            )
                .into_response();
        }
    };
    req.extensions_mut().insert(authenticated);
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DataDirGuard {
        previous: Option<std::ffi::OsString>,
        _temp: tempfile::TempDir,
    }

    impl Drop for DataDirGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("COS_DATA_DIR", value),
                None => std::env::remove_var("COS_DATA_DIR"),
            }
        }
    }

    fn isolated_data_dir() -> DataDirGuard {
        let temp = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("COS_DATA_DIR");
        std::env::set_var("COS_DATA_DIR", temp.path());
        DataDirGuard {
            previous,
            _temp: temp,
        }
    }

    #[test]
    fn signed_access_token_round_trips_and_binds_uid() {
        let _lock = crate::test_env::lock_env();
        let _data = isolated_data_dir();
        let bootstrap = load_or_generate_token().unwrap();
        ensure_signing_key().unwrap();
        let issued = exchange_bootstrap_token(&bootstrap, 1001, Some(300)).unwrap();
        let verified = verify_access_token(&issued.access_token, 1001).unwrap();
        assert_eq!(verified.uid, 1001);
        assert!(verify_access_token(&issued.access_token, 1002).is_err());
        assert_eq!(issued.expires_in, 300);
    }

    #[test]
    fn rotating_keys_invalidates_existing_access_tokens() {
        let _lock = crate::test_env::lock_env();
        let _data = isolated_data_dir();
        let bootstrap = load_or_generate_token().unwrap();
        ensure_signing_key().unwrap();
        let issued = exchange_bootstrap_token(&bootstrap, 1001, None).unwrap();
        let new_bootstrap = rotate_tokens().unwrap();
        assert_ne!(bootstrap, new_bootstrap);
        assert!(verify_access_token(&issued.access_token, 1001).is_err());
        assert!(exchange_bootstrap_token(&bootstrap, 1001, None).is_err());
        assert!(exchange_bootstrap_token(&new_bootstrap, 1001, None).is_ok());
    }

    #[test]
    fn bootstrap_secret_must_be_full_strength_hex() {
        let _lock = crate::test_env::lock_env();
        let _data = isolated_data_dir();
        assert!(persist_token("abcd").is_err());
        assert!(persist_token(&"g".repeat(64)).is_err());
    }
}
