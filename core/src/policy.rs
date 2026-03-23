/// System-level permission policy for Claw OS.
///
/// Defines operation types (OpType) and trust tiers as OS-level concepts.
/// Tier 0 = highest privilege (ROOT), higher number = lower privilege.
///
/// Permission checks read the COS_SESSION env var, look up the session's
/// tier and scope from the proc registry, and enforce access control.
/// When COS_SESSION is not set, all operations are allowed (backward compatible).
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// OpType — 6 system-level operation categories
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpType {
    /// Zero side effects — observe data.
    Read,
    /// Create or modify data.
    Write,
    /// Destroy data (hard to undo).
    Delete,
    /// Execute arbitrary code (unpredictable).
    Exec,
    /// Network communication (data exfiltration risk).
    Net,
    /// Change system state (install packages, manage services).
    System,
}

// ---------------------------------------------------------------------------
// Tier helpers
// ---------------------------------------------------------------------------

/// Check whether the given trust tier allows the requested operation.
///
/// Tier 0 (ROOT)    → all OpTypes allowed
/// Tier 1 (OPERATE) → Read, Write, Delete, Exec  (no Net, no System)
/// Tier 2 (CREATE)  → Read, Write                 (no Delete, no Exec)
/// Tier 3 (OBSERVE) → Read only
fn tier_allows(tier: u8, op: OpType) -> bool {
    match tier {
        0 => true,
        1 => matches!(op, OpType::Read | OpType::Write | OpType::Delete | OpType::Exec),
        2 => matches!(op, OpType::Read | OpType::Write),
        3 => matches!(op, OpType::Read),
        _ => false, // invalid tier = deny all
    }
}

fn tier_name(tier: u8) -> &'static str {
    match tier {
        0 => "ROOT",
        1 => "OPERATE",
        2 => "CREATE",
        3 => "OBSERVE",
        _ => "UNKNOWN",
    }
}

fn min_tier_for(op: OpType) -> u8 {
    match op {
        OpType::Read => 3,
        OpType::Write => 2,
        OpType::Delete => 1,
        OpType::Exec => 1,
        OpType::Net => 0,
        OpType::System => 0,
    }
}

// ---------------------------------------------------------------------------
// Scope checking
// ---------------------------------------------------------------------------

/// Check if `path` is within the allowed `scope`.
///
/// Uses a simple canonical-path comparison: the normalized path must start
/// with the normalized scope prefix. This prevents escape via `../`.
///
/// Edge cases:
///   - scope `"/"` allows everything
///   - scope `"/den/project"` allows `"/den/project/sub/file.txt"`
pub fn path_in_scope(scope: &str, path: &str) -> bool {
    let norm_scope = normalize_path(scope);
    let norm_path = normalize_path(path);

    // Root scope allows everything.
    if norm_scope == "/" {
        return true;
    }

    // The path must either equal the scope or fall under it (separated by '/').
    if norm_path == norm_scope {
        return true;
    }

    let prefix = if norm_scope.ends_with('/') {
        norm_scope
    } else {
        format!("{}/", norm_scope)
    };

    norm_path.starts_with(&prefix)
}

/// Normalize a path by resolving `.` and `..` components without touching
/// the filesystem. This is a pure-string operation so it works in tests
/// and sandboxed environments.
fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {} // skip empty segments and current-dir markers
            ".." => {
                parts.pop(); // go up one level; silently ignore underflow
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

// ---------------------------------------------------------------------------
// Proc registry (minimal duplicate to avoid circular dependency with proc.rs)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SessionInfo {
    session_id: String,
    #[serde(default)]
    tier: Option<u8>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize, Default)]
struct Registry {
    sessions: Vec<SessionInfo>,
}

fn proc_registry_path() -> PathBuf {
    PathBuf::from(
        std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()),
    )
    .join("proc")
    .join("registry.json")
}

fn load_proc_registry(path: &PathBuf) -> Registry {
    fs::read_to_string(path)
        .ok()
        .and_then(|data| serde_json::from_str(&data).ok())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// System-level permission check.
///
/// Reads `COS_SESSION` env var, looks up the session's tier and scope
/// from the proc registry, and checks whether the requested operation
/// is allowed.
///
/// - If `COS_SESSION` is not set, permission is always granted (backward compatible).
/// - If the session is not found in the registry, permission is granted.
/// - If no tier is set on the session, permission is granted.
pub fn require(op: OpType) -> Result<(), Value> {
    let session_id = match std::env::var("COS_SESSION") {
        Ok(sid) => sid,
        Err(_) => return Ok(()), // No session context = unrestricted
    };

    let registry = load_proc_registry(&proc_registry_path());

    let session = match registry.sessions.iter().find(|s| s.session_id == session_id) {
        Some(s) => s,
        None => return Ok(()), // Session not in registry = unrestricted
    };

    let tier = match session.tier {
        Some(t) => t,
        None => return Ok(()), // No tier set = unrestricted
    };

    if !tier_allows(tier, op) {
        return Err(json!({
            "error": "permission denied",
            "session": session_id,
            "tier": tier,
            "tier_name": tier_name(tier),
            "operation": format!("{:?}", op),
            "hint": format!(
                "Session '{}' has tier {} ({}). {:?} operations require tier {} ({}) or higher.",
                session_id, tier, tier_name(tier), op,
                min_tier_for(op), tier_name(min_tier_for(op))
            ),
            "recovery": {
                "message": "Spawn this agent with a lower tier number for more permissions",
                "example": format!("cos proc spawn --tier {} --session <id> -- <command>", min_tier_for(op))
            }
        }));
    }

    Ok(())
}

/// Check scope for a specific path argument.
///
/// Call this in addition to [`require`] when the command operates on a file
/// path that must be within the session's allowed scope.
///
/// - If `COS_SESSION` is not set, the check passes.
/// - If the session has no scope, the check passes (unrestricted).
pub fn require_scope(path: &str) -> Result<(), Value> {
    let session_id = match std::env::var("COS_SESSION") {
        Ok(sid) => sid,
        Err(_) => return Ok(()),
    };

    let registry = load_proc_registry(&proc_registry_path());

    let session = match registry.sessions.iter().find(|s| s.session_id == session_id) {
        Some(s) => s,
        None => return Ok(()),
    };

    let scope = match &session.scope {
        Some(s) => s,
        None => return Ok(()), // No scope = unrestricted
    };

    if !path_in_scope(scope, path) {
        return Err(json!({
            "error": "scope violation",
            "session": session_id,
            "scope": scope,
            "path": path,
            "hint": format!(
                "Session '{}' is scoped to '{}'. Path '{}' is outside this scope.",
                session_id, scope, path
            ),
        }));
    }

    Ok(())
}

/// Returns the current session's tier, or `None` if unrestricted.
pub fn current_tier() -> Option<u8> {
    let sid = std::env::var("COS_SESSION").ok()?;
    let reg = load_proc_registry(&proc_registry_path());
    reg.sessions
        .iter()
        .find(|s| s.session_id == sid)
        .and_then(|s| s.tier)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- tier_allows --

    #[test]
    fn tier_0_allows_everything() {
        assert!(tier_allows(0, OpType::Read));
        assert!(tier_allows(0, OpType::Write));
        assert!(tier_allows(0, OpType::Delete));
        assert!(tier_allows(0, OpType::Exec));
        assert!(tier_allows(0, OpType::Net));
        assert!(tier_allows(0, OpType::System));
    }

    #[test]
    fn tier_1_denies_net_and_system() {
        assert!(tier_allows(1, OpType::Read));
        assert!(tier_allows(1, OpType::Write));
        assert!(tier_allows(1, OpType::Delete));
        assert!(tier_allows(1, OpType::Exec));
        assert!(!tier_allows(1, OpType::Net));
        assert!(!tier_allows(1, OpType::System));
    }

    #[test]
    fn tier_2_only_read_write() {
        assert!(tier_allows(2, OpType::Read));
        assert!(tier_allows(2, OpType::Write));
        assert!(!tier_allows(2, OpType::Delete));
        assert!(!tier_allows(2, OpType::Exec));
        assert!(!tier_allows(2, OpType::Net));
        assert!(!tier_allows(2, OpType::System));
    }

    #[test]
    fn tier_3_read_only() {
        assert!(tier_allows(3, OpType::Read));
        assert!(!tier_allows(3, OpType::Write));
        assert!(!tier_allows(3, OpType::Delete));
        assert!(!tier_allows(3, OpType::Exec));
        assert!(!tier_allows(3, OpType::Net));
        assert!(!tier_allows(3, OpType::System));
    }

    #[test]
    fn tier_4_denies_everything() {
        assert!(!tier_allows(4, OpType::Read));
    }

    // -- scope --

    #[test]
    fn scope_basic() {
        assert!(path_in_scope("/den", "/den/file.txt"));
        assert!(path_in_scope("/den", "/den/sub/deep/file.txt"));
        assert!(!path_in_scope("/den/project", "/den/other/file.txt"));
        assert!(path_in_scope("/", "/anything"));
    }

    #[test]
    fn scope_no_escape() {
        // ../ should not escape scope
        assert!(!path_in_scope(
            "/den/project",
            "/den/project/../secrets/key"
        ));
    }

    #[test]
    fn scope_exact_match() {
        assert!(path_in_scope("/den/project", "/den/project"));
    }

    // -- min_tier_for --

    #[test]
    fn min_tier_correctness() {
        assert_eq!(min_tier_for(OpType::Read), 3);
        assert_eq!(min_tier_for(OpType::Write), 2);
        assert_eq!(min_tier_for(OpType::Delete), 1);
        assert_eq!(min_tier_for(OpType::Exec), 1);
        assert_eq!(min_tier_for(OpType::Net), 0);
        assert_eq!(min_tier_for(OpType::System), 0);
    }

    // -- tier_name --

    #[test]
    fn tier_names() {
        assert_eq!(tier_name(0), "ROOT");
        assert_eq!(tier_name(1), "OPERATE");
        assert_eq!(tier_name(2), "CREATE");
        assert_eq!(tier_name(3), "OBSERVE");
        assert_eq!(tier_name(255), "UNKNOWN");
    }

    // -- require (no session) --

    #[test]
    fn require_no_session_allows() {
        // When COS_SESSION is not set, everything should pass.
        std::env::remove_var("COS_SESSION");
        assert!(require(OpType::Delete).is_ok());
        assert!(require(OpType::System).is_ok());
    }
}
