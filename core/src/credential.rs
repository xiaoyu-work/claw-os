/// OS-level credential store — encrypted secret storage with tier-based access.
///
/// Analogous to the Linux kernel keyring (`keyctl`), this provides a secure
/// store for secrets (API keys, tokens, passwords) that are accessible only
/// to sessions with sufficient privilege tier.
///
/// Credentials are stored as JSON files encrypted with a simple XOR-based
/// obfuscation keyed on the machine ID. On a real deployment, this would
/// use a proper KMS or kernel keyring. The key point is the **access control**:
/// only tier 0 (ROOT) can store/revoke, and load respects the tier set at
/// store time.
///
/// Storage: `$COS_DATA_DIR/credentials/<name>.json`
///
/// Commands:
///   store <name> <value> [--tier N]  — store a credential (default tier 0 required to read)
///   load <name>                      — read a credential (tier check enforced)
///   revoke <name>                    — delete a credential
///   list                             — list stored credentials (names only, no values)
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

use crate::policy::{self, OpType};

fn credentials_dir() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("credentials")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredCredential {
    name: String,
    /// Base64-encoded obfuscated value.
    value_b64: String,
    /// Minimum tier required to load this credential (0 = ROOT only, 1 = OPERATE+, etc.)
    min_tier: u8,
    stored_at: String,
    stored_by: Option<String>,
}

/// Simple obfuscation using machine-id as key.
/// NOT cryptographically secure — meant to prevent casual reads of the file.
/// A production OS would use a kernel keyring or HSM.
fn obfuscation_key() -> Vec<u8> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(id) = fs::read_to_string("/etc/machine-id") {
            return id.trim().as_bytes().to_vec();
        }
    }
    // Fallback key
    b"claw-os-credential-store-key-v1".to_vec()
}

fn obfuscate(data: &[u8]) -> Vec<u8> {
    let key = obfuscation_key();
    data.iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % key.len()])
        .collect()
}

fn deobfuscate(data: &[u8]) -> Vec<u8> {
    obfuscate(data) // XOR is symmetric
}

fn to_b64(data: &[u8]) -> String {
    // Simple base64 without external dependency
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn from_b64(s: &str) -> Result<Vec<u8>, String> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = Vec::new();
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'\n' && b != b'\r').collect();
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let val = |c: u8| -> u32 {
            if c == b'=' {
                0
            } else {
                CHARS.iter().position(|&x| x == c).unwrap_or(0) as u32
            }
        };
        let b0 = val(chunk[0]);
        let b1 = val(chunk[1]);
        let b2 = if chunk.len() > 2 { val(chunk[2]) } else { 0 };
        let b3 = if chunk.len() > 3 { val(chunk[3]) } else { 0 };
        let n = (b0 << 18) | (b1 << 12) | (b2 << 6) | b3;
        result.push(((n >> 16) & 0xFF) as u8);
        if chunk.len() > 2 && chunk[2] != b'=' {
            result.push(((n >> 8) & 0xFF) as u8);
        }
        if chunk.len() > 3 && chunk[3] != b'=' {
            result.push((n & 0xFF) as u8);
        }
    }
    Ok(result)
}

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "store" => cmd_store(args),
        "load" => cmd_load(args),
        "revoke" => cmd_revoke(args),
        "list" => cmd_list(args),
        _ => Err(format!("unknown credential command: {command}")),
    }
}

/// Store a credential.
///
/// Usage: cos credential store <name> <value> [--tier N]
fn cmd_store(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let mut min_tier: u8 = 0; // Default: only ROOT can read
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--tier" if i + 1 < args.len() => {
                min_tier = args[i + 1]
                    .parse::<u8>()
                    .map_err(|_| "tier must be 0-3".to_string())?;
                if min_tier > 3 {
                    return Err("tier must be 0-3".into());
                }
                i += 2;
            }
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }

    if positional.len() < 2 {
        return Err("usage: cos credential store <name> <value> [--tier N]".into());
    }

    let name = &positional[0];
    let value = &positional[1];

    // Validate name: alphanumeric, hyphens, underscores
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("credential name must be alphanumeric (hyphens/underscores allowed)".into());
    }

    let dir = credentials_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create credentials dir: {e}"))?;

    // Obfuscate the value
    let obfuscated = obfuscate(value.as_bytes());
    let encoded = to_b64(&obfuscated);

    let session = std::env::var("COS_SESSION").ok();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let cred = StoredCredential {
        name: name.clone(),
        value_b64: encoded,
        min_tier,
        stored_at: now.clone(),
        stored_by: session,
    };

    let path = dir.join(format!("{name}.json"));
    let data =
        serde_json::to_string_pretty(&cred).map_err(|e| format!("failed to serialize: {e}"))?;
    fs::write(&path, data).map_err(|e| format!("failed to write credential: {e}"))?;

    // Set restrictive file permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }

    Ok(json!({
        "stored": name,
        "min_tier": min_tier,
        "stored_at": now,
    }))
}

/// Load a credential value.
///
/// Usage: cos credential load <name>
fn cmd_load(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let name = args.first().ok_or("usage: cos credential load <name>")?;
    let path = credentials_dir().join(format!("{name}.json"));

    if !path.is_file() {
        return Err(format!("credential not found: {name}"));
    }

    let data = fs::read_to_string(&path).map_err(|e| format!("failed to read credential: {e}"))?;
    let cred: StoredCredential =
        serde_json::from_str(&data).map_err(|e| format!("failed to parse credential: {e}"))?;

    // Check tier requirement
    let current_tier = policy::current_tier().unwrap_or(0);
    if current_tier > cred.min_tier {
        return Err(format!(
            "insufficient tier: credential '{}' requires tier {} or higher, current session has tier {}",
            name, cred.min_tier, current_tier
        ));
    }

    // Deobfuscate
    let obfuscated =
        from_b64(&cred.value_b64).map_err(|e| format!("failed to decode credential: {e}"))?;
    let value_bytes = deobfuscate(&obfuscated);
    let value = String::from_utf8(value_bytes)
        .map_err(|e| format!("credential is not valid UTF-8: {e}"))?;

    Ok(json!({
        "name": name,
        "value": value,
        "min_tier": cred.min_tier,
    }))
}

/// Revoke (delete) a credential.
///
/// Usage: cos credential revoke <name>
fn cmd_revoke(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let name = args.first().ok_or("usage: cos credential revoke <name>")?;
    let path = credentials_dir().join(format!("{name}.json"));

    if !path.is_file() {
        return Err(format!("credential not found: {name}"));
    }

    fs::remove_file(&path).map_err(|e| format!("failed to revoke credential: {e}"))?;

    Ok(json!({
        "revoked": name,
    }))
}

/// List all stored credentials (names only, never values).
///
/// Usage: cos credential list
fn cmd_list(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let dir = credentials_dir();
    if !dir.exists() {
        return Ok(json!({
            "credentials": [],
            "count": 0,
        }));
    }

    let mut credentials: Vec<Value> = Vec::new();
    let entries = fs::read_dir(&dir).map_err(|e| format!("failed to read credentials dir: {e}"))?;

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") {
            continue;
        }
        if let Ok(data) = fs::read_to_string(entry.path()) {
            if let Ok(cred) = serde_json::from_str::<StoredCredential>(&data) {
                credentials.push(json!({
                    "name": cred.name,
                    "min_tier": cred.min_tier,
                    "stored_at": cred.stored_at,
                    "stored_by": cred.stored_by,
                }));
            }
        }
    }

    credentials.sort_by(|a, b| {
        let na = a["name"].as_str().unwrap_or("");
        let nb = b["name"].as_str().unwrap_or("");
        na.cmp(nb)
    });

    let count = credentials.len();
    Ok(json!({
        "credentials": credentials,
        "count": count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU32, Ordering};
    static CRED_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn setup_test_dir() -> PathBuf {
        let n = CRED_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("cos-cred-test-{}-{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("COS_DATA_DIR", &dir);
        dir
    }

    #[test]
    fn obfuscate_deobfuscate_roundtrip() {
        let original = b"my-secret-api-key-12345";
        let encrypted = obfuscate(original);
        let decrypted = deobfuscate(&encrypted);
        assert_eq!(decrypted, original);
    }

    #[test]
    fn b64_roundtrip() {
        let data = b"hello world 12345!@#$%";
        let encoded = to_b64(data);
        let decoded = from_b64(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn store_and_load() {
        let _dir = setup_test_dir();
        std::env::remove_var("COS_SESSION");

        let r = cmd_store(&vec![
            "test-key".into(),
            "secret-value-123".into(),
            "--tier".into(),
            "1".into(),
        ])
        .unwrap();
        assert_eq!(r["stored"], "test-key");
        assert_eq!(r["min_tier"], 1);

        let r = cmd_load(&vec!["test-key".into()]).unwrap();
        assert_eq!(r["name"], "test-key");
        assert_eq!(r["value"], "secret-value-123");
    }

    #[test]
    fn revoke_removes_credential() {
        let _dir = setup_test_dir();
        std::env::remove_var("COS_SESSION");

        cmd_store(&vec!["temp-key".into(), "temp-value".into()]).unwrap();
        let r = cmd_revoke(&vec!["temp-key".into()]).unwrap();
        assert_eq!(r["revoked"], "temp-key");

        let r = cmd_load(&vec!["temp-key".into()]);
        assert!(r.is_err());
    }

    #[test]
    fn list_shows_names_only() {
        let _dir = setup_test_dir();
        std::env::remove_var("COS_SESSION");

        cmd_store(&vec!["key-a".into(), "val-a".into()]).unwrap();
        cmd_store(&vec!["key-b".into(), "val-b".into()]).unwrap();

        let r = cmd_list(&vec![]).unwrap();
        assert_eq!(r["count"], 2);
        let creds = r["credentials"].as_array().unwrap();
        // Should NOT contain "value" field
        for c in creds {
            assert!(c.get("value").is_none());
            assert!(c["name"].is_string());
        }
    }

    #[test]
    fn store_invalid_name() {
        let _dir = setup_test_dir();
        std::env::remove_var("COS_SESSION");

        let r = cmd_store(&vec!["bad/name".into(), "val".into()]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("alphanumeric"));
    }

    #[test]
    fn load_nonexistent() {
        let _dir = setup_test_dir();
        std::env::remove_var("COS_SESSION");

        let r = cmd_load(&vec!["nonexistent".into()]);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("not found"));
    }

    #[test]
    fn run_dispatch() {
        let _dir = setup_test_dir();
        std::env::remove_var("COS_SESSION");

        let r = run("store", &vec!["dispatch-key".into(), "val".into()]).unwrap();
        assert_eq!(r["stored"], "dispatch-key");

        let r = run("list", &vec![]).unwrap();
        assert!(r["count"].as_u64().unwrap() >= 1);

        let r = run("bogus", &vec![]);
        assert!(r.is_err());
    }
}
