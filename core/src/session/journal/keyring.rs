//! The root-only MAC key the journal signs with.
//!
//! One key file per key id under `<journal>/keys/`, plus a small
//! `active.json` naming the id new records are signed with. Old keys
//! stay readable so a rotated chain still verifies end to end; a record
//! naming an id the keyring does not hold is refused rather than
//! accepted unverified.
//!
//! Every load re-checks the file the same way, because the interesting
//! attack is not "read the key" but "make the daemon sign with a key
//! the attacker also holds":
//!
//! * the path is opened by `symlink_metadata` first, so a symlink is a
//!   refusal rather than a redirect;
//! * `nlink` must be 1, so a hardlink placed by another account cannot
//!   share the inode;
//! * the mode must have no group or other bits;
//! * the owner must be the effective uid, and must be root when the
//!   process is root;
//! * the containing directory is checked the same way.
//!
//! Creation uses `create_new` with mode `0600`, so a pre-planted file
//! is a hard failure instead of being adopted. Any violation fails
//! closed: without a trustworthy key the journal refuses to append, and
//! [`crate::session::journal`] turns that into a refused mutation.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::JournalError;

/// Bytes of key material behind one key id.
const KEY_BYTES: usize = 32;

/// Ceiling on retained keys, so rotation cannot grow without bound.
const MAX_KEYS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveKey {
    schema: u32,
    key_id: String,
    rotated_at: String,
}

/// The keys this daemon may verify with, and the one it signs with.
#[derive(Debug, Clone)]
pub struct Keyring {
    active: String,
    keys: HashMap<String, [u8; KEY_BYTES]>,
}

impl Keyring {
    /// The id new records are signed under.
    pub fn active_id(&self) -> &str {
        &self.active
    }

    pub fn active_key(&self) -> &[u8] {
        self.keys
            .get(&self.active)
            .map(|key| key.as_slice())
            .unwrap_or(&[])
    }

    /// Key material for a stored record's id, or `None` when this
    /// daemon cannot verify it.
    pub fn verify_key(&self, key_id: &str) -> Option<&[u8]> {
        self.keys.get(key_id).map(|key| key.as_slice())
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

pub fn keys_dir(root: &Path) -> PathBuf {
    root.join("keys")
}

fn active_path(root: &Path) -> PathBuf {
    keys_dir(root).join("active.json")
}

fn key_path(root: &Path, key_id: &str) -> PathBuf {
    keys_dir(root).join(format!("{key_id}.key"))
}

/// Load the keyring, creating the first key if the directory is empty.
pub fn load_or_create(root: &Path) -> Result<Keyring, JournalError> {
    let dir = keys_dir(root);
    ensure_key_dir(&dir)?;

    let active = match read_active(root)? {
        Some(active) => active,
        None => {
            let key_id = mint(root)?;
            write_active(root, &key_id)?;
            key_id
        }
    };

    let mut keys = HashMap::new();
    for entry in fs::read_dir(&dir).map_err(|error| JournalError::key(&dir, error))? {
        let entry = entry.map_err(|error| JournalError::key(&dir, error))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(key_id) = name.strip_suffix(".key") else {
            continue;
        };
        if !is_key_id(key_id) {
            return Err(JournalError::Key(format!(
                "journal key directory holds an unusable file name: {name}"
            )));
        }
        keys.insert(key_id.to_string(), read_key(&path)?);
    }

    if keys.len() > MAX_KEYS {
        return Err(JournalError::Key(format!(
            "journal keyring holds {} keys; the ceiling is {MAX_KEYS}",
            keys.len()
        )));
    }
    if !keys.contains_key(&active) {
        return Err(JournalError::Key(format!(
            "journal active key {active} is missing from the keyring"
        )));
    }

    Ok(Keyring { active, keys })
}

/// Mint a new key and make it the signing key.
///
/// Existing keys stay in the ring, so records signed before the
/// rotation still verify. Returns the new id.
pub fn rotate(root: &Path) -> Result<String, JournalError> {
    ensure_key_dir(&keys_dir(root))?;
    let key_id = mint(root)?;
    write_active(root, &key_id)?;
    Ok(key_id)
}

fn mint(root: &Path) -> Result<String, JournalError> {
    let key_id = new_key_id()?;
    let path = key_path(root, &key_id);
    let material = random_bytes()?;

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| JournalError::key(&path, error))?;
    file.write_all(hex::encode(material).as_bytes())
        .map_err(|error| JournalError::key(&path, error))?;
    file.sync_all()
        .map_err(|error| JournalError::key(&path, error))?;
    drop(file);
    super::sync_dir(&keys_dir(root))?;
    Ok(key_id)
}

fn read_active(root: &Path) -> Result<Option<String>, JournalError> {
    let path = active_path(root);
    match fs::read_to_string(&path) {
        Ok(data) => {
            check_file(&path)?;
            let active: ActiveKey = serde_json::from_str(&data).map_err(|error| {
                JournalError::Key(format!("journal active key file is unreadable: {error}"))
            })?;
            if active.schema != 1 || !is_key_id(&active.key_id) {
                return Err(JournalError::Key(
                    "journal active key file names an unusable key id".to_string(),
                ));
            }
            Ok(Some(active.key_id))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(JournalError::key(&path, error)),
    }
}

fn write_active(root: &Path, key_id: &str) -> Result<(), JournalError> {
    let path = active_path(root);
    let body = serde_json::to_string(&ActiveKey {
        schema: 1,
        key_id: key_id.to_string(),
        rotated_at: chrono::Utc::now().to_rfc3339(),
    })
    .map_err(|error| JournalError::Key(format!("encode journal active key: {error}")))?;
    super::write_durable(&path, body.as_bytes())
}

fn read_key(path: &Path) -> Result<[u8; KEY_BYTES], JournalError> {
    check_file(path)?;
    let data = fs::read_to_string(path).map_err(|error| JournalError::key(path, error))?;
    let bytes = hex::decode(data.trim())
        .map_err(|_| JournalError::Key(format!("journal key {} is not hex", path.display())))?;
    if bytes.len() != KEY_BYTES {
        return Err(JournalError::Key(format!(
            "journal key {} is {} bytes; expected {KEY_BYTES}",
            path.display(),
            bytes.len()
        )));
    }
    let mut key = [0u8; KEY_BYTES];
    key.copy_from_slice(&bytes);
    Ok(key)
}

fn ensure_key_dir(dir: &Path) -> Result<(), JournalError> {
    // Create it private, but never *re-*harden one that already exists:
    // a directory somebody widened is a signal, and silently repairing
    // it would hide the only evidence that it happened.
    if !dir.exists() {
        crate::storage::ensure_private_dir(dir).map_err(|error| JournalError::key(dir, error))?;
    }
    check_dir(dir)
}

#[cfg(unix)]
fn check_dir(path: &Path) -> Result<(), JournalError> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let meta = fs::symlink_metadata(path).map_err(|error| JournalError::key(path, error))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(JournalError::Key(format!(
            "journal key directory {} is not a real directory",
            path.display()
        )));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(JournalError::Key(format!(
            "journal key directory {} is reachable by other accounts",
            path.display()
        )));
    }
    check_owner(path, meta.uid())
}

#[cfg(unix)]
fn check_file(path: &Path) -> Result<(), JournalError> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let meta = fs::symlink_metadata(path).map_err(|error| JournalError::key(path, error))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(JournalError::Key(format!(
            "journal key {} is not a regular file",
            path.display()
        )));
    }
    if meta.nlink() != 1 {
        return Err(JournalError::Key(format!(
            "journal key {} has {} links; another account may share the inode",
            path.display(),
            meta.nlink()
        )));
    }
    if meta.permissions().mode() & 0o177 != 0 {
        return Err(JournalError::Key(format!(
            "journal key {} is not private to its owner",
            path.display()
        )));
    }
    check_owner(path, meta.uid())
}

#[cfg(unix)]
fn check_owner(path: &Path, uid: u32) -> Result<(), JournalError> {
    let euid = unsafe { libc::geteuid() } as u32;
    if uid != euid {
        return Err(JournalError::Key(format!(
            "journal key material {} is owned by uid {uid}, not by this process",
            path.display()
        )));
    }
    if euid == 0 && uid != 0 {
        return Err(JournalError::Key(format!(
            "journal key material {} is not root-owned",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_dir(_path: &Path) -> Result<(), JournalError> {
    Err(JournalError::Key(
        "the session journal requires Unix ownership and mode checks".to_string(),
    ))
}

#[cfg(not(unix))]
fn check_file(_path: &Path) -> Result<(), JournalError> {
    Err(JournalError::Key(
        "the session journal requires Unix ownership and mode checks".to_string(),
    ))
}

fn is_key_id(value: &str) -> bool {
    value.len() == 16 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn new_key_id() -> Result<String, JournalError> {
    Ok(hex::encode(&random_bytes()?[..8]))
}

#[cfg(unix)]
fn random_bytes() -> Result<[u8; KEY_BYTES], JournalError> {
    use std::io::Read;

    let mut bytes = [0u8; KEY_BYTES];
    let mut source = fs::File::open("/dev/urandom")
        .map_err(|error| JournalError::Key(format!("open /dev/urandom: {error}")))?;
    source
        .read_exact(&mut bytes)
        .map_err(|error| JournalError::Key(format!("read /dev/urandom: {error}")))?;
    Ok(bytes)
}

#[cfg(not(unix))]
fn random_bytes() -> Result<[u8; KEY_BYTES], JournalError> {
    Err(JournalError::Key(
        "the session journal requires a Unix CSPRNG".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/session/journal/keyring.rs"
    ));
}
