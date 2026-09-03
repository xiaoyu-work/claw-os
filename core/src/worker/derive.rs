//! Trusted derivation of a [`LaunchPolicy`].
//!
//! This is the only place a policy is created. Everything it reads is
//! authenticated:
//!
//! * the installed manifest and the operation the launcher selected;
//! * the effective arguments the authority already bound;
//! * the capability set the authority already resolved and granted;
//! * kernel-owned paths (`cos` binary, SDK tree, App data directory).
//!
//! Nothing here reads raw worker arguments, the launcher's ambient
//! environment, or any value the worker could influence after launch.
//!
//! ## Filesystem
//!
//! Granted `Path` scopes are mounted at the *same absolute path* they
//! have on the host. That identity mapping is what makes the mount
//! contract checkable: the argument the App receives, the scope the
//! authority granted, and the path that exists inside the sandbox are
//! the same string. A grant the kernel cannot map to exactly one
//! canonical path — a bare wildcard, a scope reaching into a
//! kernel-owned root — is not mounted, so a capability check that
//! passes still finds nothing there.
//!
//! ## Network
//!
//! Only exact `host[:port]` grants become egress endpoints. A glob
//! host cannot be pinned to an address, so it grants nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::policy::{
    Endpoint, LaunchPolicy, Limits, Mount, MountClass, NetworkPolicy, SeccompProfile, StdioPlan,
    TrustTier,
};
use crate::caps::{CapSet, Scope, Verb};

/// Sandbox path of the private `/tmp`-like scratch area.
const SANDBOX_TMP: &str = "/tmp";

/// `PATH` the worker sees. Fixed, not inherited.
const SANDBOX_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Host roots a worker may never receive a bind mount of, whatever a
/// capability says. Granting one of these would hand over the very
/// material the sandbox exists to withhold.
const FORBIDDEN_ROOTS: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/boot",
    "/run/cos",
    "/run/systemd",
    "/run/user",
    "/var/lib/cos",
    "/etc/shadow",
    "/etc/gshadow",
    "/etc/sudoers",
    "/etc/sudoers.d",
    "/etc/ssh",
    "/root",
];

/// Directory names that are credential stores wherever they appear.
const FORBIDDEN_COMPONENTS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".docker",
    ".kube",
    ".config/cos",
    ".local/share/cos/credentials",
];

/// Ceiling on how many mounts one launch may derive from its granted
/// path scopes. A segment glob is expanded by enumerating the directory
/// it names, so a grant over a large tree has to be refused rather than
/// turned into thousands of binds.
const MAX_GRANTED_MOUNTS: usize = 64;

/// Ceiling on how many entries one segment glob may match.
const MAX_GLOB_MATCHES: usize = 64;

/// Everything the kernel knows about one App operation launch.
pub struct AppOperationInput<'a> {
    pub app_id: &'a str,
    /// Canonical package directory.
    pub app_dir: &'a Path,
    pub operation: &'a str,
    /// Program the runtime selection chose (interpreter or entry).
    pub program: PathBuf,
    /// Arguments after the program.
    pub argv: Vec<String>,
    /// Capabilities the authority granted this launch.
    pub caps: &'a CapSet,
    pub session_id: &'a str,
    /// `COS_DATA_DIR` for this launch.
    pub data_dir: &'a str,
    /// `COS_APPS_DIR` for this launch.
    pub apps_dir: &'a str,
    /// Extra typed environment the runtime needs (`COS_COMMAND`,
    /// `COS_ARGS_JSON`, …). Names are validated by the policy.
    pub extra_env: BTreeMap<String, String>,
    pub stdio: StdioPlan,
    /// GUI surfaces run in the desktop tier and receive a display
    /// transport; headless operations never do.
    pub desktop: bool,
    /// `(st_dev, st_ino)` of the verified package directory.
    ///
    /// The provider refuses to bind a different inode, so a package
    /// directory swapped between verification and `execve` fails the
    /// launch instead of running unverified bytes.
    pub package_identity: Option<(u64, u64)>,
    /// Absolute path plus required inode for each signed entrypoint.
    /// Each is bound over the package mount so the exact verified file
    /// is what runs, whatever the path holds by then.
    pub pinned_entries: Vec<(PathBuf, (u64, u64))>,
    /// True when nobody signed this package and it runs only because
    /// the owner granted it developer trust.
    ///
    /// The capability set was already clamped by the daemon, but the
    /// filesystem shape is decided here: developer content sees its own
    /// package read-only and its own App data partition, and no host
    /// path a capability would otherwise have mounted.
    pub developer: bool,
}

/// Everything the kernel knows about one MCP / adapter server launch.
pub struct McpServerInput<'a> {
    /// Absolute path plus required inode for each verified file the
    /// server executes or reads at startup.
    pub pinned_entries: Vec<(PathBuf, (u64, u64))>,
    pub name: &'a str,
    pub program: PathBuf,
    pub argv: Vec<String>,
    /// Working directory on the host, already checked against the
    /// owner's home by the caller.
    pub cwd: Option<PathBuf>,
    /// Typed environment the operator configured for this server.
    pub extra_env: BTreeMap<String, String>,
    pub session_id: Option<String>,
}

/// How long the worker a policy describes is going to live.
///
/// This is the whole reason there are two shapes of App session policy.
/// A reusable server is derived once and then serves calls whose
/// capability sets differ, so nothing per-call may shape it. A
/// single-call worker exists for exactly one request, so the resources
/// that request was granted *are* its policy, and both die together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionLifetime {
    /// Long-lived stdio server. Mounts and egress come from the
    /// standing grant only, which names no resource.
    Reusable,
    /// One request, one worker. Mounts and egress are derived from the
    /// exact capability set the authority bound to that request.
    SingleCall,
}

impl SessionLifetime {
    pub const fn as_str(self) -> &'static str {
        match self {
            SessionLifetime::Reusable => "reusable",
            SessionLifetime::SingleCall => "single-call",
        }
    }
}

/// Everything the kernel knows about one long-lived App session server.
///
/// An App with an `mcp` block runs its declared entry as a stdio
/// JSON-RPC peer that outlives every individual tool call. That shape
/// is the reason this is not [`AppOperationInput`]: an operation worker
/// is derived once for one bound call and dies with it, while a session
/// worker is derived once and then serves calls whose capability sets
/// differ. Only the session's *standing* grant may shape a reusable
/// sandbox; per-call authority is either brokered or given its own
/// [`SessionLifetime::SingleCall`] worker.
pub struct AppSessionInput<'a> {
    pub app_id: &'a str,
    /// Canonical package directory.
    pub app_dir: &'a Path,
    /// Program the runtime selection chose (interpreter or entry).
    pub program: PathBuf,
    /// Arguments after the program.
    pub argv: Vec<String>,
    /// For [`SessionLifetime::Reusable`], the capabilities the
    /// authority granted the *session* — used to reject a grant whose
    /// shape this launch cannot express, never to widen the sandbox.
    /// For [`SessionLifetime::SingleCall`], the exact set bound to the
    /// one request this worker exists to serve.
    pub caps: &'a CapSet,
    pub lifetime: SessionLifetime,
    pub session_id: &'a str,
    /// `COS_DATA_DIR` for this launch.
    pub data_dir: &'a str,
    /// `COS_APPS_DIR` for this launch.
    pub apps_dir: &'a str,
    /// Extra typed environment the runtime needs (`PYTHONPATH`,
    /// `COS_MCP_SERVER`, …). Names are validated by the policy.
    pub extra_env: BTreeMap<String, String>,
    /// `(st_dev, st_ino)` of the verified package directory.
    pub package_identity: Option<(u64, u64)>,
    /// Absolute path plus required inode for the signed session entry
    /// and the manifest that selected it.
    pub pinned_entries: Vec<(PathBuf, (u64, u64))>,
    /// Desktop transports a kernel-side classification granted this
    /// App. Empty for everything a manifest can describe; see
    /// [`super::trusted_desktop`].
    pub transports: &'a [super::trusted_desktop::Transport],
}

/// A model-authored command run through the agent sandbox tool.
pub struct AgentExecInput {
    pub workspace: PathBuf,
    pub writable: bool,
    pub argv: Vec<String>,
    pub endpoints: Vec<Endpoint>,
    pub limits: Limits,
}

/// Derive the policy for one App operation (or GUI surface).
pub fn app_operation(input: AppOperationInput<'_>) -> Result<LaunchPolicy, String> {
    let tier = if input.desktop {
        TrustTier::DesktopSurface
    } else {
        TrustTier::AppOperation
    };
    let app_dir = canonical_dir(input.app_dir, "App package")?;
    // The launcher hands over the *owner's* data root, which holds the
    // credential store, the session registry and the journal. A worker
    // gets its own partition of it instead: one directory per App,
    // created `0700`, and `COS_DATA_DIR` points there. Everything an
    // App writes — including the agent-memory rows the kernel already
    // scopes to `self:<app>` — lands inside its own partition, and no
    // other App's or owner's store is in the sandbox at all.
    let data_dir = app_partition(Path::new(input.data_dir), input.app_id)?;

    let package_mount = match input.package_identity {
        Some(identity) => Mount::read_only(app_dir.clone(), app_dir.clone(), MountClass::Package)
            .expecting(identity),
        None => Mount::read_only(app_dir.clone(), app_dir.clone(), MountClass::Package),
    };
    let mut mounts = vec![
        package_mount,
        Mount::read_write(data_dir.clone(), data_dir.clone(), MountClass::AppData),
    ];
    // Bind each signed entrypoint over the package mount by inode. The
    // package directory mount alone would still let a file inside it be
    // replaced after verification; pinning the entries closes that.
    for (path, identity) in &input.pinned_entries {
        if path == &app_dir {
            continue;
        }
        mounts.push(
            Mount::read_only(path.clone(), path.clone(), MountClass::Package).expecting(*identity),
        );
    }
    mounts.extend(runtime_mounts());
    mounts.extend(shared_library_mounts(&app_dir, Path::new(input.apps_dir)));
    mounts.extend(program_mount(&input.program));
    if !input.developer {
        mounts.extend(granted_path_mounts(input.caps)?);
    }
    let mut env = base_env(&data_dir);
    if input.desktop {
        let (display_mounts, display_env) = desktop_transports();
        mounts.extend(display_mounts);
        env.extend(display_env);
    }
    dedupe_mounts(&mut mounts);

    let network = egress_from_caps(input.caps);
    let seccomp = seccomp_for(&network);
    env.insert("COS_APP_ID".to_string(), input.app_id.to_string());
    env.insert("COS_SESSION".to_string(), input.session_id.to_string());
    env.insert(
        "COS_DATA_DIR".to_string(),
        data_dir.to_string_lossy().into_owned(),
    );
    env.insert("COS_APPS_DIR".to_string(), input.apps_dir.to_string());
    for (name, value) in input.extra_env {
        env.insert(name, value);
    }
    apply_egress_env(&mut env, &network);

    let limits = if input.desktop {
        Limits::desktop()
    } else {
        Limits::operation()
    };
    Ok(LaunchPolicy {
        tier,
        label: format!("app:{}/{}", input.app_id, input.operation),
        program: input.program,
        argv: input.argv,
        workdir: data_dir,
        mounts,
        network,
        env,
        limits,
        seccomp,
        stdio: input.stdio,
        broker: true,
        umask: 0o077,
    })
}

/// Derive the policy for one MCP server or adapter.
///
/// MCP servers are third-party code with no manifest-granted
/// capabilities of their own, so they get no host paths beyond the
/// read-only system image and no egress at all.
pub fn mcp_server(input: McpServerInput<'_>) -> Result<LaunchPolicy, String> {
    let mut mounts = runtime_mounts();
    mounts.extend(program_mount(&input.program));
    for (path, identity) in &input.pinned_entries {
        mounts.push(
            Mount::read_only(path.clone(), path.clone(), MountClass::Package).expecting(*identity),
        );
    }
    let workdir = match &input.cwd {
        Some(cwd) => {
            let cwd = canonical_dir(cwd, "MCP working directory")?;
            reject_forbidden(&cwd)?;
            mounts.push(Mount::read_only(
                cwd.clone(),
                cwd.clone(),
                MountClass::Package,
            ));
            cwd
        }
        None => PathBuf::from(SANDBOX_TMP),
    };
    dedupe_mounts(&mut mounts);

    let mut env = base_env(Path::new(SANDBOX_TMP));
    if let Some(session) = &input.session_id {
        env.insert("COS_SESSION".to_string(), session.clone());
    }
    for (name, value) in input.extra_env {
        env.insert(name, value);
    }
    Ok(LaunchPolicy {
        tier: TrustTier::McpServer,
        label: format!("mcp:{}", input.name),
        program: input.program,
        argv: input.argv,
        workdir,
        mounts,
        network: NetworkPolicy::Denied,
        env,
        limits: Limits::server(),
        seccomp: SeccompProfile::Strict,
        stdio: StdioPlan::Streamed,
        broker: input.session_id.is_some(),
        umask: 0o077,
    })
}

/// Derive the policy for one App session server.
///
/// The shape is the MCP-server shape — stdio JSON-RPC, the strict
/// syscall filter — with the package material an App owns: its verified
/// directory read-only and pinned by inode, its signed entrypoints
/// bound over that directory, its own partition of the data root, and
/// the `_shared` helper trees a bundled App imports.
///
/// ## Two lifetimes, one derivation
///
/// A [`SessionLifetime::Reusable`] worker is launched once and then
/// serves many tool calls, each with its own capability set. Mounts and
/// egress rules cannot be revised on a live worker, so deriving them
/// from *any* per-call set would leave the first call's filesystem and
/// network reach standing for every later call — including calls the
/// authority denied. The standing grant a session holds is
/// `agent.invoke` on itself, which names no path and no host, so a
/// reusable policy mounts no host path and opens no egress at all. A
/// standing grant that *would* need one is refused rather than
/// honoured.
///
/// A [`SessionLifetime::SingleCall`] worker exists for exactly one
/// request. Its mounts and egress come from the capability set the
/// authority bound to that request, and it is destroyed with the
/// response, so nothing it was granted can outlive the call or be
/// observed by another one.
///
/// ## Transports
///
/// `transports` is non-empty only for the fixed vendor Apps
/// [`super::trusted_desktop::classify`] recognises. It lifts the tier
/// to [`TrustTier::TrustedDesktopSession`] and binds exactly the named
/// sockets — never the directory holding them.
pub fn app_session(input: AppSessionInput<'_>) -> Result<LaunchPolicy, String> {
    let app_dir = canonical_dir(input.app_dir, "App package")?;
    let data_dir = app_partition(Path::new(input.data_dir), input.app_id)?;

    if matches!(input.lifetime, SessionLifetime::Reusable) {
        reject_standing_resource_grants(input.caps, input.app_id)?;
    }

    let package_mount = match input.package_identity {
        Some(identity) => Mount::read_only(app_dir.clone(), app_dir.clone(), MountClass::Package)
            .expecting(identity),
        None => Mount::read_only(app_dir.clone(), app_dir.clone(), MountClass::Package),
    };
    let mut mounts = vec![
        package_mount,
        Mount::read_write(data_dir.clone(), data_dir.clone(), MountClass::AppData),
    ];
    for (path, identity) in &input.pinned_entries {
        if path == &app_dir {
            continue;
        }
        mounts.push(
            Mount::read_only(path.clone(), path.clone(), MountClass::Package).expecting(*identity),
        );
    }
    mounts.extend(runtime_mounts());
    mounts.extend(shared_library_mounts(&app_dir, Path::new(input.apps_dir)));
    mounts.extend(program_mount(&input.program));

    // Only a single-call worker turns capabilities into filesystem and
    // network reach, because only a single-call worker dies before the
    // next call can inherit it.
    let network = match input.lifetime {
        SessionLifetime::Reusable => NetworkPolicy::Denied,
        SessionLifetime::SingleCall => {
            mounts.extend(granted_path_mounts(input.caps)?);
            egress_from_caps(input.caps)
        }
    };

    let tier = if input.transports.is_empty() {
        TrustTier::McpServer
    } else {
        TrustTier::TrustedDesktopSession
    };
    let mut env = base_env(&data_dir);
    let (transport_mounts, transport_env) =
        super::trusted_desktop::transport_mounts(input.transports);
    mounts.extend(transport_mounts);
    env.extend(transport_env);
    dedupe_mounts(&mut mounts);

    env.insert("COS_APP_ID".to_string(), input.app_id.to_string());
    env.insert("COS_SESSION".to_string(), input.session_id.to_string());
    env.insert(
        "COS_DATA_DIR".to_string(),
        data_dir.to_string_lossy().into_owned(),
    );
    env.insert("COS_APPS_DIR".to_string(), input.apps_dir.to_string());
    for (name, value) in input.extra_env {
        env.insert(name, value);
    }
    apply_egress_env(&mut env, &network);

    let seccomp = seccomp_for(&network);
    Ok(LaunchPolicy {
        tier,
        label: format!("app-session:{}", input.app_id),
        program: input.program,
        argv: input.argv,
        workdir: data_dir,
        mounts,
        network,
        env,
        // A server is torn down with its handle; a single-call worker is
        // bounded by the call it serves.
        limits: match input.lifetime {
            SessionLifetime::Reusable => Limits::server(),
            SessionLifetime::SingleCall => Limits::operation(),
        },
        seccomp,
        stdio: StdioPlan::Streamed,
        broker: true,
        umask: 0o077,
    })
}

/// Refuse a session grant this launch shape cannot express.
///
/// A filesystem or network capability in a *standing* session grant
/// would have to become a mount or an egress rule to mean anything, and
/// a long-lived worker cannot have either revised later. Rather than
/// widen the initial policy — which every subsequent call would inherit
/// — the launch fails closed and says why. The scope shape does not
/// change that: a wildcard filesystem grant held at rest is a broader
/// claim than a path one, not a narrower one.
fn reject_standing_resource_grants(caps: &CapSet, app_id: &str) -> Result<(), String> {
    for cap in caps.iter() {
        let resource = match cap.verb {
            Verb::FS_READ
            | Verb::FS_WRITE
            | Verb::FS_DELETE
            | Verb::FS_META
            | Verb::FS_WATCH
            | Verb::FS_EXEC => "filesystem",
            Verb::NET_DIAL | Verb::NET_LISTEN => "network",
            _ => match &cap.scope {
                Scope::Path(_) => "filesystem",
                Scope::Host(_) => "network",
                _ => continue,
            },
        };
        return Err(format!(
            "App `{app_id}` session holds a standing {resource} grant (`{}`); \
             session workers receive resources per call through the broker, \
             so a standing grant cannot be honoured without widening every \
             later call's sandbox",
            cap.verb.as_str()
        ));
    }
    Ok(())
}

/// Derive the policy for a model-authored command.
pub fn agent_exec(input: AgentExecInput) -> Result<LaunchPolicy, String> {
    let workspace = canonical_dir(&input.workspace, "sandbox workspace")?;
    reject_forbidden(&workspace)?;
    let mut mounts = runtime_mounts();
    mounts.push(if input.writable {
        Mount::read_write(workspace.clone(), workspace.clone(), MountClass::Output)
    } else {
        Mount::read_only(workspace.clone(), workspace.clone(), MountClass::Input)
    });

    let network = if input.endpoints.is_empty() {
        NetworkPolicy::Denied
    } else {
        NetworkPolicy::Brokered {
            endpoints: sorted_endpoints(input.endpoints),
        }
    };
    let mut env = base_env(&workspace);
    apply_egress_env(&mut env, &network);
    let seccomp = seccomp_for(&network);
    let mut argv = input.argv;
    if argv.is_empty() {
        return Err("sandbox exec requires a command".to_string());
    }
    let program = resolve_program(&argv.remove(0))?;
    mounts.extend(program_mount(&program));
    dedupe_mounts(&mut mounts);
    Ok(LaunchPolicy {
        tier: TrustTier::AgentExec,
        label: "agent:exec".to_string(),
        program,
        argv,
        workdir: workspace,
        mounts,
        network,
        env,
        limits: input.limits,
        seccomp,
        stdio: StdioPlan::Captured,
        broker: false,
        umask: 0o077,
    })
}

/// Display and session transports for the desktop tier.
///
/// This is the one place a worker can be handed a compositor socket, a
/// session bus or a GPU node, and it is reachable only from
/// [`TrustTier::DesktopSurface`] — a headless operation worker cannot
/// opt into it from a manifest. Each transport is bound individually:
/// the runtime directory that contains them is never exposed, so an
/// App sees its own compositor socket and not its neighbours' sockets,
/// keyrings or agent endpoints.
fn desktop_transports() -> (Vec<Mount>, BTreeMap<String, String>) {
    let mut mounts = Vec::new();
    let mut env = BTreeMap::new();
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);

    if let (Some(runtime_dir), Ok(display)) = (&runtime_dir, std::env::var("WAYLAND_DISPLAY")) {
        // A display name with a separator would name a socket outside
        // the runtime directory.
        if !display.is_empty() && !display.contains('/') && !display.contains("..") {
            let socket = runtime_dir.join(&display);
            if socket.exists() {
                mounts.push(Mount::read_write(
                    socket.clone(),
                    socket,
                    MountClass::Display,
                ));
                env.insert("WAYLAND_DISPLAY".to_string(), display);
                env.insert(
                    "XDG_RUNTIME_DIR".to_string(),
                    runtime_dir.to_string_lossy().into_owned(),
                );
            }
        }
    }
    if let Ok(xauthority) = std::env::var("XAUTHORITY") {
        let path = PathBuf::from(&xauthority);
        if path.is_file() {
            mounts.push(Mount::read_only(path.clone(), path, MountClass::Display));
            env.insert("XAUTHORITY".to_string(), xauthority);
            if let Ok(display) = std::env::var("DISPLAY") {
                env.insert("DISPLAY".to_string(), display);
            }
        }
    }
    // GPU nodes: a window that cannot reach a renderer is not a window.
    // `--dev-bind` is the only mount in the whole policy that enables
    // device access, and only for this tier.
    let dri = PathBuf::from("/dev/dri");
    if dri.is_dir() {
        mounts.push(Mount::read_write(dri.clone(), dri, MountClass::Device));
    }
    (mounts, env)
}

/// The `_shared` helper trees a bundled App imports.
///
/// Bundled Apps put their package's parent on `sys.path` and import
/// `_shared.safe_http`, `_shared.credentials` and friends — the very
/// helpers that hold the safe filesystem, subprocess and egress
/// behaviour. Mounting only the App's own directory would leave every
/// one of them unimportable, so the sibling `_shared` and the apps
/// root's `_shared` come with it, read-only. Nothing else from the apps
/// root is exposed: a neighbouring App stays invisible.
fn shared_library_mounts(app_dir: &Path, apps_root: &Path) -> Vec<Mount> {
    let mut candidates = Vec::new();
    if let Some(parent) = app_dir.parent() {
        candidates.push(parent.join("_shared"));
    }
    candidates.push(apps_root.join("_shared"));
    let mut mounts = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for candidate in candidates {
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if !canonical.is_dir() || !seen.insert(canonical.clone()) {
            continue;
        }
        mounts.push(Mount::read_only(
            canonical.clone(),
            canonical,
            MountClass::Runtime,
        ));
    }
    mounts
}

/// Bind the program itself when it lives outside the read-only system
/// image.
///
/// An interpreter installed under `/opt`, `~/.nvm` or a version
/// manager is the normal case for Node, and the sandbox's system image
/// only covers `/usr`. The binary is bound read-only and on its own:
/// its directory is not exposed, so a sibling script in the same
/// `bin/` stays invisible.
fn program_mount(program: &Path) -> Option<Mount> {
    const SYSTEM_ROOTS: &[&str] = &["/usr/", "/bin/", "/sbin/", "/lib/", "/lib64/"];
    let text = program.to_string_lossy();
    if SYSTEM_ROOTS.iter().any(|root| text.starts_with(root)) {
        return None;
    }
    program.is_file().then(|| {
        Mount::read_only(
            program.to_path_buf(),
            program.to_path_buf(),
            MountClass::Runtime,
        )
    })
}

/// Read-only kernel assets every worker needs: the `cos` binary it
/// shells out to and the SDK/runtime Python trees the wrapper imports.
fn runtime_mounts() -> Vec<Mount> {
    let mut mounts = Vec::new();
    if let Some(binary) = cos_binary() {
        mounts.push(Mount::read_only(
            binary.clone(),
            binary,
            MountClass::Runtime,
        ));
    }
    for candidate in sdk_roots() {
        mounts.push(Mount::read_only(
            candidate.clone(),
            candidate,
            MountClass::Runtime,
        ));
    }
    mounts
}

fn cos_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("COS_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path.canonicalize().ok();
        }
    }
    std::env::current_exe().ok().and_then(|exe| {
        let sibling = exe.parent()?.join("cos");
        sibling.canonicalize().ok().filter(|path| path.is_file())
    })
}

fn sdk_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = std::env::var_os("COS_SDK_PYTHON_DIR") {
        roots.push(PathBuf::from(dir));
    }
    roots.push(PathBuf::from("/usr/lib/cos/python"));
    roots
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| path.is_dir())
        .collect()
}

/// Environment shared by every tier. Deliberately tiny: a hostile
/// worker starts from nothing and receives only what it is named.
fn base_env(home: &Path) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), SANDBOX_PATH.to_string());
    env.insert("HOME".to_string(), home.to_string_lossy().into_owned());
    env.insert("TMPDIR".to_string(), SANDBOX_TMP.to_string());
    env.insert("LANG".to_string(), "C.UTF-8".to_string());
    env.insert("LC_ALL".to_string(), "C.UTF-8".to_string());
    env.insert("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string());
    env.insert("PYTHONNOUSERSITE".to_string(), "1".to_string());
    // Non-interactive by construction: a sandboxed worker has no
    // terminal and must never wait for one.
    env.insert("DEBIAN_FRONTEND".to_string(), "noninteractive".to_string());
    env.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());
    env.insert("CI".to_string(), "true".to_string());
    env.insert("PAGER".to_string(), "cat".to_string());
    env.insert("GIT_PAGER".to_string(), "cat".to_string());
    env.insert("PIP_NO_INPUT".to_string(), "1".to_string());
    env.insert("NPM_CONFIG_YES".to_string(), "true".to_string());
    // Enforcement inside the sandbox is advisory — the authority is the
    // broker — but it must still refuse by default.
    env.insert("COS_PERMS_MODE".to_string(), "strict".to_string());
    env.insert(super::SANDBOX_MARKER_ENV.to_string(), "1".to_string());
    if let Some(binary) = cos_binary() {
        env.insert("COS_BIN".to_string(), binary.to_string_lossy().into_owned());
    }
    if let Some(sdk) = sdk_roots().into_iter().next() {
        env.insert(
            "COS_SDK_PYTHON_DIR".to_string(),
            sdk.to_string_lossy().into_owned(),
        );
    }
    if let Some(path) = sdk_python_path() {
        env.insert("PYTHONPATH".to_string(), path);
    }
    env
}

/// Point standard proxy variables at the brokered egress endpoint so a
/// stock HTTP client works without knowing anything about the sandbox.
fn apply_egress_env(env: &mut BTreeMap<String, String>, network: &NetworkPolicy) {
    let NetworkPolicy::Brokered { endpoints } = network else {
        return;
    };
    env.insert(
        "COS_EGRESS_SOCKET".to_string(),
        super::linux_egress_socket().to_string(),
    );
    env.insert(
        "COS_EGRESS_ENDPOINTS".to_string(),
        endpoints
            .iter()
            .map(Endpoint::authority)
            .collect::<Vec<_>>()
            .join(","),
    );
}

fn seccomp_for(network: &NetworkPolicy) -> SeccompProfile {
    match network {
        NetworkPolicy::Denied => SeccompProfile::Strict,
        _ => SeccompProfile::StrictNetwork,
    }
}

/// Turn granted `Path` capabilities into bind mounts.
///
/// The mapping is exact, never approximate. `*` matches one path
/// segment, so a `Documents/*` grant is expanded by *enumerating*
/// `Documents` and binding each match at its own depth — mounting
/// `Documents` itself would hand over every grandchild the grant does
/// not cover. `**` covers a subtree and may be mounted as a prefix, but
/// only after the subtree has been checked for a forbidden descendant,
/// so `$HOME/**` is refused rather than quietly exposing `~/.ssh`.
///
/// Write grants are never expanded: a glob names a set, and a set has
/// no canonical target to create into. A write scope must name one
/// exact path (or one whose parent exists).
pub fn granted_path_mounts(caps: &CapSet) -> Result<Vec<Mount>, String> {
    let mut writable: BTreeSet<PathBuf> = BTreeSet::new();
    let mut readable: BTreeSet<PathBuf> = BTreeSet::new();

    for cap in caps.iter() {
        let Scope::Path(pattern) = &cap.scope else {
            continue;
        };
        let write = matches!(cap.verb, Verb::FS_WRITE | Verb::FS_DELETE);
        let read = matches!(
            cap.verb,
            Verb::FS_READ | Verb::FS_META | Verb::FS_WATCH | Verb::FS_EXEC
        );
        if !write && !read {
            continue;
        }
        if write {
            if let Some(resolved) = resolve_write_target(pattern)? {
                writable.insert(resolved);
            }
        } else {
            for resolved in resolve_read_targets(pattern)? {
                readable.insert(resolved);
            }
        }
    }

    let mut mounts = Vec::with_capacity(writable.len() + readable.len());
    for path in &writable {
        mounts.push(Mount::read_write(
            path.clone(),
            path.clone(),
            MountClass::Output,
        ));
    }
    for path in readable {
        // A path that is already writable must not be re-bound
        // read-only, and a read-only bind must not shadow a writable
        // one: least privilege here would silently remove access the
        // grant gave, so the writable mount wins and the read-only
        // duplicate is dropped.
        if writable.contains(&path) {
            continue;
        }
        mounts.push(Mount::read_only(path.clone(), path, MountClass::Input));
    }
    if mounts.len() > MAX_GRANTED_MOUNTS {
        return Err(format!(
            "granted paths expand to {} mounts, above the {MAX_GRANTED_MOUNTS} the sandbox allows",
            mounts.len()
        ));
    }
    Ok(mounts)
}

/// Resolve a write scope to the one canonical path it names.
///
/// A recursive `**` scope names a subtree and is honoured like a read
/// one, after the same forbidden-descendant check. A single-segment `*`
/// or a partial glob is refused: there would be no single target to
/// create into, and picking a prefix would grant write access to
/// everything under it.
fn resolve_write_target(pattern: &str) -> Result<Option<PathBuf>, String> {
    let expanded = expand_home(pattern);
    if !expanded.starts_with('/') {
        return Ok(None);
    }
    let segments: Vec<&str> = expanded
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let literal_depth = segments
        .iter()
        .position(|segment| segment.contains('*') || segment.contains('?'))
        .unwrap_or(segments.len());
    if literal_depth == 0 {
        return Ok(None);
    }
    let rest = &segments[literal_depth..];
    let recursive = match rest.first().copied() {
        None => false,
        Some("**") if rest.len() == 1 => true,
        _ => {
            return Err(format!(
                "write scope `{pattern}` is ambiguous: a write grant must name an exact \
                 path or a `**` subtree"
            ));
        }
    };

    let prefix = PathBuf::from(format!("/{}", segments[..literal_depth].join("/")));
    // A write grant for a file that does not exist yet is normal; the
    // parent directory is what has to be mounted.
    let target = if prefix.exists() {
        prefix
    } else if recursive {
        return Ok(None);
    } else {
        let Some(parent) = prefix.parent().map(Path::to_path_buf) else {
            return Ok(None);
        };
        if !parent.exists() {
            return Ok(None);
        }
        parent
    };
    let canonical = target
        .canonicalize()
        .map_err(|error| format!("resolve granted path `{pattern}`: {error}"))?;
    reject_forbidden(&canonical)?;
    if recursive {
        reject_forbidden_descendants(&canonical)?;
    }
    reject_special(&canonical)?;
    Ok(Some(canonical))
}

/// Resolve a read scope to every canonical path it names.
fn resolve_read_targets(pattern: &str) -> Result<Vec<PathBuf>, String> {
    let expanded = expand_home(pattern);
    if !expanded.starts_with('/') {
        return Ok(Vec::new());
    }
    let segments: Vec<&str> = expanded
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let literal_depth = segments
        .iter()
        .position(|segment| segment.contains('*') || segment.contains('?'))
        .unwrap_or(segments.len());
    if literal_depth == 0 {
        // A grant rooted at `/` cannot be honoured as a mount without
        // handing over the host, so it maps to nothing.
        return Ok(Vec::new());
    }
    let prefix = PathBuf::from(format!("/{}", segments[..literal_depth].join("/")));
    if !prefix.exists() {
        return Ok(Vec::new());
    }
    let prefix = prefix
        .canonicalize()
        .map_err(|error| format!("resolve granted path `{pattern}`: {error}"))?;
    reject_forbidden(&prefix)?;

    let rest = &segments[literal_depth..];
    match rest.first().copied() {
        // No glob at all: the scope names one path.
        None => {
            reject_special(&prefix)?;
            Ok(vec![prefix])
        }
        // `**` covers the whole subtree, so the prefix itself is the
        // mount — but only if nothing forbidden lives beneath it.
        Some("**") if rest.len() == 1 => {
            reject_forbidden_descendants(&prefix)?;
            reject_special(&prefix)?;
            Ok(vec![prefix])
        }
        // `*` is exactly one segment. Enumerate it: mounting the parent
        // would expose every grandchild the grant does not cover.
        Some("*") if rest.len() == 1 => enumerate_segment(&prefix, pattern),
        // Anything deeper (`a/*/b`, `**/c`, `*.txt`) has no single
        // canonical expansion the sandbox can prove, so it grants
        // nothing rather than something wider.
        _ => Ok(Vec::new()),
    }
}

/// Bind every direct child of `parent`, at its own depth.
fn enumerate_segment(parent: &Path, pattern: &str) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(parent)
        .map_err(|error| format!("expand granted path `{pattern}`: {error}"))?;
    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("expand granted path `{pattern}`: {error}"))?;
        let path = entry.path();
        // A symlink child is not followed: the grant covers the entry
        // in this directory, not wherever it points.
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !(metadata.is_dir() || metadata.is_file()) {
            continue;
        }
        if reject_forbidden(&path).is_err() {
            continue;
        }
        matches.push(path);
        if matches.len() > MAX_GLOB_MATCHES {
            return Err(format!(
                "granted path `{pattern}` matches more than {MAX_GLOB_MATCHES} entries"
            ));
        }
    }
    Ok(matches)
}

fn is_glob(value: &str) -> bool {
    value.contains('*') || value.contains('?')
}

/// Make the SDK importable from any Python entry in the sandbox.
///
/// The bundled `main.py` wrapper extends `sys.path` itself, but a
/// worker that runs `python3 -c`, an adapter, or a helper subprocess
/// needs the same tree — and `PYTHONPATH` is the only way to say so
/// before the interpreter starts.
fn sdk_python_path() -> Option<String> {
    let roots = sdk_roots();
    if roots.is_empty() {
        return None;
    }
    Some(
        roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// Refuse a subtree mount that would expose a credential store or a
/// kernel-owned root somewhere beneath it.
///
/// Checking only the prefix's own components is not enough: `$HOME/**`
/// has no forbidden component, and yet covers `~/.ssh`. The scan is
/// bounded — it looks for the known store names at the depths they
/// actually occur — so a deep tree does not turn a launch into a walk
/// of the whole filesystem.
fn reject_forbidden_descendants(prefix: &Path) -> Result<(), String> {
    for component in FORBIDDEN_COMPONENTS {
        if prefix.join(component).exists() {
            return Err(format!(
                "recursive scope would expose credential store `{component}`"
            ));
        }
    }
    for root in FORBIDDEN_ROOTS {
        let root = Path::new(root);
        if root.starts_with(prefix) && root != prefix {
            return Err(format!(
                "recursive scope would expose kernel-owned path `{}`",
                root.display()
            ));
        }
    }
    Ok(())
}

fn expand_home(pattern: &str) -> String {
    let Some(rest) = pattern.strip_prefix('~') else {
        return pattern.to_string();
    };
    let home = crate::paths::current_home_override()
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_default();
    format!("{home}{rest}")
}

/// Refuse to mount anything inside a kernel-owned or credential root.
/// This is a hard failure, not a silent skip: a launch that believes it
/// was granted `~/.ssh` must not proceed as if it had been.
pub fn reject_forbidden(path: &Path) -> Result<(), String> {
    let text = path.to_string_lossy();
    for root in FORBIDDEN_ROOTS {
        if text == *root || text.starts_with(&format!("{root}/")) {
            return Err(format!(
                "worker sandbox refuses to expose kernel-owned path `{root}`"
            ));
        }
    }
    for component in FORBIDDEN_COMPONENTS {
        if text.contains(&format!("/{component}/")) || text.ends_with(&format!("/{component}")) {
            return Err(format!(
                "worker sandbox refuses to expose credential store `{component}`"
            ));
        }
    }
    Ok(())
}

/// Only directories and regular files are mountable. A socket, FIFO,
/// device node or anything else is a transport, not data, and binding
/// one would reintroduce exactly the ambient access the sandbox
/// removes.
fn reject_special(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspect worker mount source: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("worker mount source resolved to a symlink".to_string());
    }
    if !(metadata.is_dir() || metadata.is_file()) {
        return Err("worker mount source is not a directory or regular file".to_string());
    }
    Ok(())
}

/// Turn granted `Host` capabilities into exact egress endpoints.
pub fn egress_from_caps(caps: &CapSet) -> NetworkPolicy {
    let mut endpoints = Vec::new();
    for cap in caps.iter() {
        if !matches!(cap.verb, Verb::NET_DIAL) {
            continue;
        }
        let Scope::Host(pattern) = &cap.scope else {
            continue;
        };
        // A glob host cannot be resolved to a pinned address, so it
        // grants nothing: the broker would have no identity to check.
        if pattern.contains('*') {
            continue;
        }
        let (host, port) = match pattern.rsplit_once(':') {
            Some((host, port)) => match port.parse::<u16>() {
                Ok(port) => (host, port),
                Err(_) => (pattern.as_str(), 443),
            },
            None => (pattern.as_str(), 443),
        };
        let endpoint = Endpoint::new(host, port);
        if super::policy::validate_endpoint(&endpoint).is_ok() {
            endpoints.push(endpoint);
        }
    }
    if endpoints.is_empty() {
        NetworkPolicy::Denied
    } else {
        NetworkPolicy::Brokered {
            endpoints: sorted_endpoints(endpoints),
        }
    }
}

fn sorted_endpoints(mut endpoints: Vec<Endpoint>) -> Vec<Endpoint> {
    endpoints.sort();
    endpoints.dedup();
    endpoints
}

fn dedupe_mounts(mounts: &mut Vec<Mount>) {
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    mounts.retain(|mount| seen.insert(mount.target.clone()));
}

fn canonical_dir(path: &Path, what: &str) -> Result<PathBuf, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("resolve {what}: {error}"))?;
    if !canonical.is_dir() {
        return Err(format!("{what} is not a directory"));
    }
    Ok(canonical)
}

fn ensure_dir(path: &Path, what: &str) -> Result<PathBuf, String> {
    if !path.exists() {
        std::fs::create_dir_all(path).map_err(|error| format!("create {what}: {error}"))?;
    }
    canonical_dir(path, what)
}

/// One App's private slice of the owner's data root.
///
/// `<data-root>/apps/<app-id>`, created `0700` so a same-uid process
/// outside the sandbox cannot browse it either. The App id has already
/// been validated by the manifest loader, and it is re-checked here
/// because this value becomes a path component.
///
/// Every filesystem call is checked: a partition that cannot be
/// created, cannot be restricted, or does not come back as a
/// directory owned by this account with no group or world bits fails
/// the launch. Silently continuing would hand the worker a directory
/// somebody else can read.
fn app_partition(data_root: &Path, app_id: &str) -> Result<PathBuf, String> {
    if app_id.is_empty()
        || app_id.len() > 64
        || !app_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || app_id.starts_with('.')
    {
        return Err(format!("App id `{app_id}` is not a safe path component"));
    }
    let partition = data_root.join("apps").join(app_id);
    let created = ensure_dir(&partition, "App data partition")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        std::fs::set_permissions(&created, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                format!(
                    "restrict App data partition `{}`: {error}",
                    created.display()
                )
            },
        )?;
        let meta = std::fs::symlink_metadata(&created).map_err(|error| {
            format!(
                "inspect App data partition `{}`: {error}",
                created.display()
            )
        })?;
        if !meta.file_type().is_dir() {
            return Err(format!(
                "App data partition `{}` is not a directory",
                created.display()
            ));
        }
        let effective = unsafe { libc::geteuid() };
        if meta.uid() != effective {
            return Err(format!(
                "App data partition `{}` belongs to uid {} rather than {effective}",
                created.display(),
                meta.uid()
            ));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "App data partition `{}` is readable beyond its owner",
                created.display()
            ));
        }
    }
    super::migrate::migrate_legacy_state(data_root, &created, app_id)?;
    Ok(created)
}

fn resolve_program(program: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(program);
    if candidate.is_absolute() {
        return candidate
            .canonicalize()
            .map_err(|error| format!("resolve sandbox program `{program}`: {error}"));
    }
    for root in SANDBOX_PATH.split(':') {
        let path = Path::new(root).join(program);
        if path.is_file() {
            return path
                .canonicalize()
                .map_err(|error| format!("resolve sandbox program `{program}`: {error}"));
        }
    }
    Err(format!("sandbox program `{program}` was not found"))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/worker/derive.rs"
    ));
}
