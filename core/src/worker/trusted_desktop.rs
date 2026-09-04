//! Kernel-side allowlist for the bundled native Desktop Apps whose MCP
//! service is implemented by a root-owned system program rather than by
//! a file the package ships.
//!
//! Every row names one App id and the one absolute program its manifest
//! `mcp.entry` may point at. Naming a program outside the package is the
//! *first* thing a row grants, and for most rows it is the only thing:
//! those Apps run as ordinary hostile [`McpServer`](super::TrustTier::McpServer)
//! workers with no desktop transport at all.
//!
//! Three of them additionally reach the desktop over the **session
//! bus**, because their tool surface is a bus call:
//!
//! | App | What the tool actually does |
//! | --- | --- |
//! | `cosmic-player` | `zbus::Connection::session()` → MPRIS2 on the active player |
//! | `cosmic-screenshot` | `ashpd` → `xdg-desktop-portal`, then `org.freedesktop.Notifications` |
//! | `cosmic-notifications` | `zbus::Connection::session()` → `org.freedesktop.Notifications` |
//!
//! None of them initialises a compositor connection in MCP mode — each
//! `main()` returns into the MCP server before libcosmic is touched —
//! so no Wayland socket, no X authority and no GPU node is granted
//! here. The session bus alone is what makes the difference between a
//! working tool and a syscall failure.
//!
//! ## This is an expanded TCB, deliberately and narrowly
//!
//! The session bus is not a narrow capability. A process holding that
//! socket can talk to every service the owner's session exposes, and
//! Claw OS has no way to filter method calls inside it. That is the
//! cost of the transport, and it is why reaching this classification
//! requires *all* of:
//!
//! 1. the App id is one of the fixed rows in [`ALLOWLIST`], which is
//!    kernel source, not configuration;
//! 2. the package verified through **vendor** provenance —
//!    package-manager trust under an approved system root — so a
//!    publisher signature over a package that merely calls itself
//!    `cosmic-player` is refused;
//! 3. the package directory sits under an approved vendor root *and*
//!    every component of it is root-owned, non-symlink and not
//!    group/world-writable;
//! 4. the artifact that will actually be executed is root-owned,
//!    non-symlink, not group/world-writable, and is byte-for-byte the
//!    absolute path this table names for that App id.
//!
//! A manifest field, a developer grant, a signed third-party package, a
//! symlink or bind alias onto an approved root, and the App id on its
//! own are each insufficient. Anything that fails returns `None`, and a
//! manifest that named a system program then has no way to launch at
//! all — the caller refuses rather than running an unclassified binary.
//! A row that carries no transport never gains one from any of this:
//! the transport list is source, per row, and empty is the default.

use std::path::{Path, PathBuf};

use super::policy::{Mount, MountClass};

/// One desktop transport a classified session may hold.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Transport {
    /// The owner's session message bus (`DBUS_SESSION_BUS_ADDRESS`).
    SessionBus,
}

impl Transport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Transport::SessionBus => "session-bus",
        }
    }
}

/// One kernel-side row. Everything in it is source, never input.
struct Row {
    app_id: &'static str,
    /// The absolute program this App's `mcp.entry` may name. Exact, and
    /// the only path outside the package this App id can ever execute.
    system_program: &'static str,
    /// Desktop transports the row grants. Empty for every App whose
    /// tools do not need one, which is most of them.
    transports: &'static [Transport],
}

/// No transport at all: the row exists only to let this App id name its
/// own system program.
const NO_TRANSPORT: &[Transport] = &[];

/// The owner's session bus, and nothing else.
const SESSION_BUS: &[Transport] = &[Transport::SessionBus];

const ALLOWLIST: &[Row] = &[
    Row {
        app_id: "cosmic-files",
        system_program: "/usr/bin/cosmic-files",
        transports: NO_TRANSPORT,
    },
    Row {
        app_id: "cosmic-edit",
        system_program: "/usr/bin/cosmic-edit",
        transports: NO_TRANSPORT,
    },
    Row {
        app_id: "cosmic-store",
        system_program: "/usr/bin/cosmic-store",
        transports: NO_TRANSPORT,
    },
    Row {
        app_id: "cosmic-settings",
        system_program: "/usr/bin/cosmic-settings",
        transports: NO_TRANSPORT,
    },
    Row {
        app_id: "cosmic-term",
        system_program: "/usr/bin/cosmic-term",
        transports: NO_TRANSPORT,
    },
    Row {
        app_id: "cosmic-launcher",
        system_program: "/usr/bin/cosmic-launcher",
        transports: NO_TRANSPORT,
    },
    Row {
        app_id: "cosmic-player",
        system_program: "/usr/bin/cosmic-player",
        transports: SESSION_BUS,
    },
    Row {
        app_id: "cosmic-screenshot",
        system_program: "/usr/bin/cosmic-screenshot",
        transports: SESSION_BUS,
    },
    Row {
        app_id: "cosmic-notifications",
        system_program: "/usr/bin/cosmic-notifications",
        transports: SESSION_BUS,
    },
];

/// What a classified session was granted. Produced only by
/// [`classify`]; there is no public constructor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopGrant {
    transports: Vec<Transport>,
}

impl DesktopGrant {
    pub fn transports(&self) -> &[Transport] {
        &self.transports
    }
}

/// The absolute program an App's manifest may name outside its package.
///
/// Consulted by the MCP-entry check so the bundled native Desktop Apps
/// can point at the root-owned binary that implements their tool
/// surface. It answers only for the fixed rows above, so no other App
/// can name a path outside its package.
pub fn allowlisted_system_program(app_id: &str) -> Option<&'static str> {
    ALLOWLIST
        .iter()
        .find(|row| row.app_id == app_id)
        .map(|row| row.system_program)
}

/// Classify one App session launch.
///
/// `program` is the artifact the runtime selection resolved — for every
/// row here, the desktop binary itself.
///
/// Returns `None` — never an error — when anything fails. The caller
/// refuses a manifest that names a system program without this
/// classification, regardless of whether the fixed row carries a
/// desktop transport.
pub fn classify(
    app_id: &str,
    package: &crate::provenance::VerifiedPackage,
    program: &Path,
) -> Option<DesktopGrant> {
    let row = ALLOWLIST.iter().find(|row| row.app_id == app_id)?;
    // Vendor provenance, not a signature. A publisher-signed package
    // that calls itself `cosmic-player` never reaches the transport.
    if !matches!(package.source(), crate::provenance::TrustSource::Vendor) {
        return None;
    }
    if !crate::provenance::verify::is_vendor_root_path(package.dir()) {
        return None;
    }
    // An approved root is a prefix test; this is the ownership test the
    // prefix does not make. A bind mount or symlink chain that lands a
    // user-writable tree under `/usr/lib/cos` fails here.
    let dir = package.dir().canonicalize().ok()?;
    crate::provenance::fsec::require_secure_location(&dir, &[0]).ok()?;

    // What will execute is root-owned and immutable to the owner, and
    // is exactly the path this row names. Exact path equality against
    // kernel source, after canonicalisation, so an alias resolving to
    // the same inode by a different name is still refused.
    root_owned(program)?;
    let expected = Path::new(row.system_program).canonicalize().ok()?;
    if program.canonicalize().ok()? != expected {
        return None;
    }
    Some(DesktopGrant {
        transports: row.transports.to_vec(),
    })
}

/// Root-owned, non-symlink, not group/world-writable, with every
/// ancestor the same.
fn root_owned(path: &Path) -> Option<()> {
    let canonical = path.canonicalize().ok()?;
    crate::provenance::fsec::require_secure_location(&canonical, &[0]).ok()?;
    Some(())
}

/// Path the classified session bus appears at inside the sandbox.
///
/// Fixed and private: the worker is told this, not the host path it
/// came from. Nothing about the owner's runtime directory — its uid,
/// its layout — crosses the boundary, and two launches for different
/// owners look identical from inside.
pub const SANDBOX_SESSION_BUS: &str = "/run/cos/session-bus";

/// Bind mounts and environment for the granted transports.
///
/// Each transport is resolved from *authenticated* facts — the launch's
/// owner uid and that uid's systemd runtime directory — and bound
/// individually at a fixed sandbox path. The directory holding it is
/// never exposed, so a classified session sees its session bus and not
/// the owner's keyring, agent or compositor sockets sitting beside it.
///
/// Anything that does not resolve grants nothing. There is no fallback
/// mount and no "best effort" address: a transport the launcher cannot
/// authenticate is a transport the worker does not get, and its tools
/// then fail with a clear error instead of reaching something else.
pub fn transport_mounts(
    transports: &[Transport],
) -> (Vec<Mount>, std::collections::BTreeMap<String, String>) {
    let mut mounts = Vec::new();
    let mut env = std::collections::BTreeMap::new();
    for transport in transports {
        match transport {
            Transport::SessionBus => {
                let socket = match session_bus_socket() {
                    Ok(socket) => socket,
                    Err(BusRefusal(reason)) => {
                        tracing::warn!(
                            target: "cos_app",
                            %reason,
                            "no session bus transport for this launch"
                        );
                        continue;
                    }
                };
                // Pinned by inode like every other authenticated mount:
                // the provider re-opens it `O_PATH|O_NOFOLLOW` and
                // refuses a different inode, so a swap between this
                // check and `execve` fails the launch.
                mounts.push(
                    Mount::read_write(
                        socket.path,
                        PathBuf::from(SANDBOX_SESSION_BUS),
                        MountClass::Display,
                    )
                    .expecting(socket.identity),
                );
                env.insert(
                    "DBUS_SESSION_BUS_ADDRESS".to_string(),
                    format!("unix:path={SANDBOX_SESSION_BUS}"),
                );
            }
        }
    }
    (mounts, env)
}

/// Stable identity of the transports a launch would actually get.
///
/// Not the same question as [`DesktopGrant::label`], which says what
/// the classification *allows*. This says which inode the launch would
/// be bound to, so a session whose bus socket was replaced — the login
/// session restarted, the socket recreated — is not handed back from
/// the cache still holding a descriptor on the old one.
///
/// An unavailable transport is part of the identity too: a session that
/// came up without a bus must not be reused once one appears, because
/// its worker has no mount for it.
pub fn transport_fingerprint(transports: &[Transport]) -> String {
    transports
        .iter()
        .map(|transport| match transport {
            Transport::SessionBus => match session_bus_socket() {
                Ok(socket) => format!(
                    "{}@{}:{}",
                    transport.as_str(),
                    socket.identity.0,
                    socket.identity.1
                ),
                Err(_) => format!("{}@unavailable", transport.as_str()),
            },
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A session-bus socket that passed every check.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedSocket {
    path: PathBuf,
    identity: (u64, u64),
}

/// Why an address was refused. Carried so the caller can log one
/// reason; it never changes the outcome, which is always "no
/// transport".
#[derive(Clone, Debug, Eq, PartialEq)]
struct BusRefusal(String);

/// The owner's session bus, if this launch can authenticate one.
fn session_bus_socket() -> Result<ResolvedSocket, BusRefusal> {
    let uid = owner_uid()?;
    let runtime = owner_runtime_dir(uid)?;
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .map_err(|_| BusRefusal("no session bus address is set".to_string()))?;
    let declared = parse_unix_bus_address(&address)?;
    verify_bus_socket(&declared, &runtime, uid)
}

/// The uid this launch runs for.
///
/// The routed override when the launcher is acting for another account,
/// its own effective uid otherwise. Never a value from the
/// environment, and never root: a session bus belongs to a login
/// session, and root's is not something to hand to hostile code.
fn owner_uid() -> Result<u32, BusRefusal> {
    #[cfg(unix)]
    {
        let uid = match crate::paths::current_owner_uid_override() {
            Some(uid) => uid,
            None => unsafe { libc::geteuid() as u32 },
        };
        if uid == 0 {
            return Err(BusRefusal(
                "refusing to bind root's session bus".to_string(),
            ));
        }
        Ok(uid)
    }
    #[cfg(not(unix))]
    {
        Err(BusRefusal("session transports require Unix".to_string()))
    }
}

/// The runtime directory that uid's session bus must live in.
///
/// `/run/user/<uid>` is the systemd fact and is tried first.
/// `XDG_RUNTIME_DIR` is consulted only when that does not verify, and
/// only through the same checks — it is an environment variable, so it
/// is a hint about *where to look*, never evidence that what is found
/// there belongs to the owner.
fn owner_runtime_dir(uid: u32) -> Result<PathBuf, BusRefusal> {
    let systemd = PathBuf::from(format!("/run/user/{uid}"));
    if let Ok(dir) = verify_runtime_dir(&systemd, uid) {
        return Ok(dir);
    }
    let Some(declared) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return Err(BusRefusal(format!(
            "{} is not a usable runtime directory for uid {uid}",
            systemd.display()
        )));
    };
    verify_runtime_dir(Path::new(&declared), uid)
}

/// A runtime directory is only the owner's when the kernel says so.
///
/// Owned by the uid, a real directory rather than a symlink, private to
/// that uid, and every ancestor root-owned and not group/world-writable
/// so nobody else can swap a component underneath it.
fn verify_runtime_dir(dir: &Path, uid: u32) -> Result<PathBuf, BusRefusal> {
    if !dir.is_absolute() {
        return Err(BusRefusal("runtime directory is not absolute".to_string()));
    }
    if dir
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(BusRefusal("runtime directory contains `..`".to_string()));
    }
    let meta = crate::provenance::fsec::lstat(dir)
        .map_err(|error| BusRefusal(format!("{}: {error}", dir.display())))?;
    if meta.is_symlink || !meta.is_dir {
        return Err(BusRefusal(format!(
            "{} is not a real directory",
            dir.display()
        )));
    }
    if meta.uid != uid {
        return Err(BusRefusal(format!(
            "{} belongs to uid {}, not {uid}",
            dir.display(),
            meta.uid
        )));
    }
    // The bus socket itself is world-accessible by convention; the
    // directory mode is what actually keeps other accounts out, so it
    // is the mode that has to be private.
    if meta.mode & 0o077 != 0 {
        return Err(BusRefusal(format!(
            "{} is mode {:o}, not private to its owner",
            dir.display(),
            meta.mode
        )));
    }
    // Ancestors must be root-owned: a user-writable `/run` or
    // `/run/user` would let another account rename the directory in.
    let mut cursor = dir.parent();
    while let Some(ancestor) = cursor {
        let ancestor_meta = crate::provenance::fsec::lstat(ancestor)
            .map_err(|error| BusRefusal(format!("{}: {error}", ancestor.display())))?;
        if ancestor_meta.is_symlink || !ancestor_meta.is_dir {
            return Err(BusRefusal(format!(
                "{} is not a real directory",
                ancestor.display()
            )));
        }
        if ancestor_meta.uid != 0 {
            return Err(BusRefusal(format!(
                "{} is not root-owned",
                ancestor.display()
            )));
        }
        let sticky = ancestor_meta.mode & 0o1000 != 0;
        if ancestor_meta.is_group_or_world_writable() && !sticky {
            return Err(BusRefusal(format!(
                "{} is group- or world-writable",
                ancestor.display()
            )));
        }
        cursor = ancestor.parent();
    }
    Ok(dir.to_path_buf())
}

/// Socket file names a session bus may have inside the runtime
/// directory. One entry, because systemd creates exactly one.
const BUS_SOCKET_NAMES: &[&str] = &["bus"];

/// Parse `DBUS_SESSION_BUS_ADDRESS` as a single `unix:path=` address.
///
/// The D-Bus address grammar is a `;`-separated list of alternatives,
/// each `transport:key=value,key=value`, with values percent-encoded.
/// Almost all of that flexibility is a liability here: an alternative
/// list means the launcher and the worker could pick different
/// endpoints, `abstract=` names a socket in a network namespace the
/// sandbox does not share, and `dir=`/`tmpdir=` ask the client to
/// invent a path. So: exactly one alternative, exactly the `unix`
/// transport, exactly one filesystem `path`, and nothing else.
fn parse_unix_bus_address(raw: &str) -> Result<PathBuf, BusRefusal> {
    let alternatives: Vec<&str> = raw.split(';').filter(|part| !part.is_empty()).collect();
    if alternatives.len() != 1 {
        return Err(BusRefusal(format!(
            "expected exactly one bus address, found {}",
            alternatives.len()
        )));
    }
    let (transport, options) = alternatives[0]
        .split_once(':')
        .ok_or_else(|| BusRefusal("bus address has no transport".to_string()))?;
    if transport != "unix" {
        return Err(BusRefusal(format!(
            "bus transport `{transport}` is not a filesystem socket"
        )));
    }
    let mut path: Option<String> = None;
    let mut seen_guid = false;
    for option in options.split(',').filter(|part| !part.is_empty()) {
        let (key, value) = option
            .split_once('=')
            .ok_or_else(|| BusRefusal(format!("bus option `{option}` has no value")))?;
        match key {
            "path" => {
                if path.is_some() {
                    return Err(BusRefusal("bus address repeats `path`".to_string()));
                }
                path = Some(percent_decode(value)?);
            }
            "guid" => {
                if seen_guid {
                    return Err(BusRefusal("bus address repeats `guid`".to_string()));
                }
                seen_guid = true;
            }
            // `abstract` is worth naming: it is a valid unix address
            // that this sandbox structurally cannot honour, and
            // treating it as merely absent would be confusing.
            "abstract" => {
                return Err(BusRefusal(
                    "bus address is abstract; the sandbox owns a private network \
                     namespace and cannot reach an abstract socket"
                        .to_string(),
                ));
            }
            other => {
                return Err(BusRefusal(format!(
                    "bus address carries unsupported option `{other}`"
                )));
            }
        }
    }
    let path =
        PathBuf::from(path.ok_or_else(|| BusRefusal("bus address names no path".to_string()))?);
    if !path.is_absolute() {
        return Err(BusRefusal("bus path is not absolute".to_string()));
    }
    Ok(path)
}

/// Strict `%XX` decoding. A stray `%`, a short escape or a non-hex
/// digit is a malformed address, not something to pass through.
fn percent_decode(value: &str) -> Result<String, BusRefusal> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes
                .get(index + 1..index + 3)
                .ok_or_else(|| BusRefusal("bus path has a truncated escape".to_string()))?;
            let text = std::str::from_utf8(hex)
                .map_err(|_| BusRefusal("bus path has a malformed escape".to_string()))?;
            let byte = u8::from_str_radix(text, 16)
                .map_err(|_| BusRefusal("bus path has a non-hex escape".to_string()))?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    if out.iter().any(|byte| *byte == 0 || byte.is_ascii_control()) {
        return Err(BusRefusal(
            "bus path contains a NUL or control byte".to_string(),
        ));
    }
    String::from_utf8(out).map_err(|_| BusRefusal("bus path is not UTF-8".to_string()))
}

/// The declared path has to be *this* owner's bus, and has to be a
/// socket, checked without following a symlink at any step.
fn verify_bus_socket(
    declared: &Path,
    runtime: &Path,
    uid: u32,
) -> Result<ResolvedSocket, BusRefusal> {
    if declared
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(BusRefusal("bus path contains `..`".to_string()));
    }
    // Kernel-owned endpoints are never a session bus, whatever an
    // address claims. The parent check below already excludes them;
    // naming them here means a future runtime-directory change cannot
    // quietly make one reachable.
    for reserved in [
        crate::paths::clawd_socket_path(),
        crate::paths::runtime_dir(),
        PathBuf::from(super::linux_egress_socket()),
        PathBuf::from("/run/systemd"),
    ] {
        if declared == reserved || declared.starts_with(&reserved) {
            return Err(BusRefusal(format!(
                "{} is a kernel-owned endpoint, not a session bus",
                declared.display()
            )));
        }
    }
    let parent = declared
        .parent()
        .ok_or_else(|| BusRefusal("bus path has no parent".to_string()))?;
    if parent != runtime {
        return Err(BusRefusal(format!(
            "{} is outside the owner's runtime directory {}",
            declared.display(),
            runtime.display()
        )));
    }
    let name = declared
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| BusRefusal("bus path has no file name".to_string()))?;
    if !BUS_SOCKET_NAMES.contains(&name) {
        return Err(BusRefusal(format!(
            "`{name}` is not a session bus socket name"
        )));
    }
    let meta = crate::provenance::fsec::lstat(declared)
        .map_err(|error| BusRefusal(format!("{}: {error}", declared.display())))?;
    if meta.is_symlink {
        return Err(BusRefusal(format!("{} is a symlink", declared.display())));
    }
    if !meta.is_socket {
        return Err(BusRefusal(format!(
            "{} is not a Unix socket",
            declared.display()
        )));
    }
    if meta.uid != uid {
        return Err(BusRefusal(format!(
            "{} belongs to uid {}, not {uid}",
            declared.display(),
            meta.uid
        )));
    }
    Ok(ResolvedSocket {
        path: declared.to_path_buf(),
        identity: (meta.dev, meta.ino),
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/trusted_desktop.rs"
    ));
}
