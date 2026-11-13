use super::*;
use std::os::unix::fs::MetadataExt;

const OBSERVER: &str = "/run/user/62050/native-product-observer/window.json";
const TARGET: &str = "/run/display-home/native-editor-startup.txt";
const CONTENT: &[u8] =
    b"Actual product-owned Editor startup.\nMetadata consent is not content consent.\n";

mod editor_save_as {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/server/gui_fixture/editor_save_as.rs"
    ));
}

mod terminal_pty {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/server/gui_fixture/terminal_pty.rs"
    ));
}

mod calendar_popup {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/server/gui_fixture/calendar_popup.rs"
    ));
}

mod store_catalog {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/server/gui_fixture/store_catalog.rs"
    ));
}
mod clipboard_history {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/server/gui_fixture/clipboard_history.rs"
    ));
}

#[derive(Clone, Copy)]
pub(super) struct Product {
    pub(super) name: &'static str,
    pub(super) app: &'static str,
    pub(super) case: usize,
    desktop_id: &'static str,
    entries: &'static [&'static str],
    ungranted: &'static [usize],
    home_metadata: bool,
    selector: &'static str,
    panel: bool,
    interactive_panel: bool,
    observation: Option<&'static str>,
}

pub(super) const EDITOR: Product = Product {
    name: "editor",
    app: "cosmic-edit",
    case: 25,
    desktop_id: "com.clawos.Edit",
    entries: &["bin/cosmic-edit"],
    ungranted: &[0, 1, 2],
    home_metadata: true,
    selector: "--gui",
    panel: false,
    interactive_panel: false,
    observation: None,
};
pub(super) const FILES: Product = Product {
    name: "files",
    app: "cosmic-files",
    case: 26,
    desktop_id: "com.clawos.Files",
    entries: &["bin/cosmic-files", "bin/cosmic-files-applet"],
    ungranted: &[0, 1, 2, 3],
    home_metadata: true,
    selector: "--gui",
    panel: false,
    interactive_panel: false,
    observation: None,
};
pub(super) const TERMINAL: Product = Product {
    name: "terminal",
    app: "cosmic-term",
    case: 27,
    desktop_id: "com.clawos.Term",
    entries: &["bin/cosmic-term"],
    ungranted: &[],
    home_metadata: false,
    selector: "--gui",
    panel: false,
    interactive_panel: false,
    observation: None,
};
pub(super) const CALENDAR: Product = Product {
    name: "calendar",
    app: "panel-calendar",
    case: 28,
    desktop_id: "com.clawos.PanelCalendarButton",
    entries: &["bin/claw-applet-calendar"],
    ungranted: &[],
    home_metadata: false,
    selector: "open",
    panel: true,
    interactive_panel: false,
    observation: None,
};
pub(super) const STORE: Product = Product {
    name: "store",
    app: "cosmic-store",
    case: 29,
    desktop_id: "com.clawos.Store",
    entries: &["bin/cosmic-store"],
    ungranted: &[0],
    home_metadata: false,
    selector: "--gui",
    panel: false,
    interactive_panel: false,
    observation: Some("packages"),
};
pub(super) const WIDGETS: Product = Product {
    name: "desktop-widgets",
    app: "widget-rail",
    case: 30,
    desktop_id: "com.clawos.AppletWidgetRail",
    entries: &["bin/claw-applet-widget-rail"],
    ungranted: &[],
    home_metadata: false,
    selector: "open",
    panel: true,
    interactive_panel: false,
    observation: None,
};
pub(super) const CLIPBOARD: Product = Product {
    name: "clipboard",
    app: "panel-clipboard",
    case: 31,
    desktop_id: "com.clawos.AppletClipboard",
    entries: &["bin/claw-applet-clipboard"],
    ungranted: &[],
    home_metadata: false,
    selector: "open",
    panel: true,
    interactive_panel: false,
    observation: None,
};
pub(super) const LAUNCHER: Product = Product {
    name: "launcher",
    app: "cosmic-launcher",
    case: 32,
    desktop_id: "com.clawos.Launcher",
    entries: &["bin/cosmic-launcher"],
    ungranted: &[],
    home_metadata: false,
    selector: "--gui",
    panel: true,
    interactive_panel: true,
    observation: None,
};

impl Product {
    pub(super) fn for_case(case: usize) -> Option<Self> {
        [EDITOR, FILES, TERMINAL, CALENDAR, STORE, WIDGETS, CLIPBOARD, LAUNCHER]
            .into_iter().find(|product| product.case == case)
    }

    fn startup_needs(self) -> Vec<Cap> {
        if self.interactive_panel {
            return vec![Cap::unscoped(Verb::UI_PANEL)];
        }
        if self.name == "clipboard" {
            return vec![
                Cap::new(Verb::CLIPBOARD_READ, Scope::name("history")),
                Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("history")),
                Cap::new(Verb::CLIPBOARD_WRITE, Scope::name("selection")),
                Cap::unscoped(Verb::UI_PANEL),
            ];
        }
        let mut needs = Vec::new();
        if self.panel {
            needs.push(Cap::new(Verb::DATA_DB_READ, Scope::name("calendar")));
            if self.name == "desktop-widgets" {
                needs.push(Cap::new(Verb::SYS_OBSERVE, Scope::name("resources")));
                needs.push(Cap::new(Verb::AGENT_OBSERVE, Scope::name("tasks")));
            }
            needs.push(Cap::unscoped(Verb::UI_PANEL));
        }
        if let Some(domain) = self.observation {
            needs.push(Cap::new(Verb::SYS_OBSERVE, Scope::name(domain)));
        }
        needs
    }

    fn directory(self) -> std::path::PathBuf {
        Path::new("/usr/lib/cos/apps").join(self.app)
    }

    fn entry(self) -> std::path::PathBuf {
        self.directory().join(self.entries[0])
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    schema: String,
    source_lock: serde_json::Value,
    platform_lock: serde_json::Value,
    entrypoints: Vec<String>,
    resources: Vec<String>,
    payload_inventory: Vec<crate::provenance::envelope::FileEntry>,
}

pub(super) fn install(key: &crate::provenance::sign::SigningKeyFile, product: Product) {
    use crate::provenance::{
        envelope::PackageKind,
        sign::{sign_directory, SignRequest},
        verify::{verify_package, VerifyOptions},
    };
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let needs = product.startup_needs();
    let has_selection = |verb| needs.iter().any(|cap| {
        cap.verb == verb && cap.scope == Scope::name("selection")
    });
    let mut projection = std::fs::OpenOptions::new().write(true).create_new(true)
        .mode(0o644).custom_flags(libc::O_NOFOLLOW)
        .open("/run/gui-public/native-product-selection.json").unwrap();
    projection.set_permissions(std::fs::Permissions::from_mode(0o644)).unwrap();
    projection.write_all(&serde_json::to_vec(&serde_json::json!({
        "schema":"claw.private-native-selection/v2",
        "selection_read":has_selection(Verb::CLIPBOARD_READ),
        "selection_write":has_selection(Verb::CLIPBOARD_WRITE),
        "interactive_layer":product.interactive_panel,
    })).unwrap()).unwrap();
    drop(projection);

    let receipt = Path::new("/run/gui-fixture").join(format!("native-{}.json", product.name));
    let metadata = std::fs::symlink_metadata(&receipt).unwrap();
    assert!(metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0);
    assert!(metadata.len() <= 2 * 1024 * 1024);
    let mut payload: Payload = serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
    assert_eq!(
        payload.schema,
        format!("claw.private-native-{}/v1", product.name)
    );
    assert_eq!(payload.source_lock["version"], 1);
    let revision = payload.source_lock["revision"].as_str().unwrap();
    assert!(
        revision.len() == 40
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    let selection: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../packaging/apps.lock.json"
    )))
    .unwrap();
    assert_eq!(payload.source_lock["repository"], selection["repository"]);
    assert_eq!(payload.platform_lock["version"], "0.2.1");
    assert_eq!(payload.platform_lock["runtime_abi"], 1);
    assert_eq!(
        payload.platform_lock["url"],
        "https://github.com/xiaoyu-work/claw-os/releases/download/app-platform-v0.2.1/claw-os-app-platform-0.2.1.tar.gz"
    );
    let platform_digest = payload.platform_lock["sha256"].as_str().unwrap();
    assert!(
        platform_digest.len() == 64
            && platform_digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    if product.name == "terminal" || product.panel || product.observation.is_some() {
        assert_eq!(
            platform_digest,
            "0412f750add06a32c81312c9405338687471249a01dcbf0c9688e66885dde8d8"
        );
    }
    assert_eq!(payload.entrypoints, product.entries);
    let required = if matches!(product.name, "desktop-widgets" | "clipboard") {
        vec![format!("resources/usr/share/applications/{}.desktop", product.desktop_id)]
    } else if product.panel && !product.interactive_panel {
        vec![
            format!(
                "resources/usr/share/applications/{}.desktop",
                product.desktop_id
            ),
            format!(
                "resources/usr/share/icons/hicolor/scalable/actions/{}-symbolic.svg",
                product.desktop_id
            ),
        ]
    } else {
        vec![
            format!(
                "resources/usr/share/applications/{}.desktop",
                product.desktop_id
            ),
            format!(
                "resources/usr/share/metainfo/{}.metainfo.xml",
                product.desktop_id
            ),
            format!(
                "resources/usr/share/icons/hicolor/scalable/apps/{}.svg",
                product.desktop_id
            ),
        ]
    };
    for required in required {
        assert!(
            payload
                .resources
                .iter()
                .any(|resource| resource == &required)
        );
    }
    let dir = product.directory();
    assert!(
        !dir.join(".provenance.json").exists(),
        "fixture input was already signed"
    );
    for entry in product.entries {
        assert_eq!(
            std::fs::read(dir.join(entry)).unwrap()[..7],
            *b"\x7fELF\x02\x01\x01"
        );
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("app.json")).unwrap()).unwrap();
    assert_eq!(manifest["id"], product.app);
    assert_eq!(manifest["schema_version"], 2);
    sign_directory(
        &dir,
        &SignRequest {
            kind: PackageKind::App,
            id: product.app.into(),
            version: manifest["version"].as_str().unwrap().into(),
            manifest_schema: "cos.app-manifest/v2".into(),
            manifest_path: "app.json".into(),
            entrypoints: payload.entrypoints,
            resources: payload.resources,
        },
        key,
    )
    .unwrap();
    std::fs::set_permissions(
        dir.join(".provenance.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let package = verify_package(
        &dir,
        &VerifyOptions::new(PackageKind::App).expect_id(product.app),
        &crate::provenance::trust_store(),
    )
    .unwrap();
    let mut signed = package.files().cloned().collect::<Vec<_>>();
    signed.sort_by(|left, right| left.path.cmp(&right.path));
    payload
        .payload_inventory
        .sort_by(|left, right| left.path.cmp(&right.path));
    assert_eq!(
        signed, payload.payload_inventory,
        "signed App must retain every actual installer/resource/license byte and mode"
    );
    let review = crate::approvals::system_review::submit(
        OWNER,
        crate::approvals::system_review::ReviewKind::AppActivation,
        &package,
        format!("private real {} acceptance controller", product.name),
    )
    .unwrap();
    crate::approvals::system_review::decide(OWNER, &review.id, true, Some(&package)).unwrap();
    println!(
        "private actual {} input: App revision {revision}, public platform {platform_digest}, signed ELF/resources/licenses verified",
        product.name
    );
}

fn diagnostic(stage: &str) -> String {
    let log = std::fs::read("/run/gui-fixture/pam-native-product.log").unwrap_or_default();
    let tail = &log[log.len().saturating_sub(8192)..];
    format!(
        "actual native gate {stage}: {}{}",
        String::from_utf8_lossy(tail),
        private_audit_tail()
    )
}

fn observe() -> Option<serde_json::Value> {
    match std::fs::symlink_metadata(OBSERVER) {
        Ok(metadata) => {
            assert!(
                metadata.is_file()
                    && metadata.uid() == OWNER
                    && metadata.mode() & 0o077 == 0
                    && metadata.len() <= 8192
            );
            Some(serde_json::from_slice(&std::fs::read(OBSERVER).unwrap()).unwrap())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read protected compositor observation: {error}"),
    }
}

fn buffered_window(record: &serde_json::Value, instance: &str) -> bool {
    record["schema"] == "claw.private-native-window/v1"
        && record["instance"] == instance
        && record["toplevels"] == 1
        && record["configure_acked"] == true
        && record["closed"] == false
        && record["buffer_commits"]
            .as_u64()
            .is_some_and(|count| count > 0)
        && record["raster"]["width"]
            .as_u64()
            .is_some_and(|size| size >= 100)
        && record["raster"]["height"]
            .as_u64()
            .is_some_and(|size| size >= 100)
        && record["raster"]["nonuniform_pixels"] == true
}

fn buffered_panel(record: &serde_json::Value, instance: &str, namespace: &str) -> bool {
    buffered_panel_size(record, instance, namespace, 16..=512, 16..=128, "None")
}

fn buffered_widget_panel(record: &serde_json::Value, instance: &str) -> bool {
    buffered_panel_size(record, instance, WIDGETS.desktop_id, 100..=512, 100..=800, "None")
}

fn buffered_palette(record: &serde_json::Value, instance: &str) -> bool {
    buffered_panel_size(record, instance, "launcher", 100..=1024, 100..=800, "Exclusive")
}

fn buffered_panel_size(
    record: &serde_json::Value, instance: &str, namespace: &str,
    width: std::ops::RangeInclusive<u64>, height: std::ops::RangeInclusive<u64>,
    keyboard: &str,
) -> bool {
    record["schema"] == "claw.private-native-panel/v1"
        && record["instance"] == instance
        && record["role"] == "zwlr_layer_surface_v1"
        && record["namespace"] == namespace
        && record["layer"] == "Top"
        && record["layer_surfaces"] == 1
        && record["toplevels"] == 0
        && record["keyboard_interactivity"] == keyboard
        && record["configure_acked"] == true
        && record["closed"] == false
        && record["buffer_commits"]
            .as_u64()
            .is_some_and(|count| count > 0)
        && record["raster"]["width"]
            .as_u64()
            .is_some_and(|size| width.contains(&size))
        && record["raster"]["height"]
            .as_u64()
            .is_some_and(|size| height.contains(&size))
        && record["configure_size"][0] == record["raster"]["width"]
        && record["configure_size"][1] == record["raster"]["height"]
        && record["raster"]["nonuniform_pixels"] == true
}

fn loaded(record: &serde_json::Value) -> bool {
    record["title"]
        .as_str()
        .is_some_and(|title| title.starts_with("native-editor-startup.txt - "))
}

struct NativeProcess {
    identity: ProcessIdentity,
    pidfd: OwnedFd,
    inode: (u64, u64),
}

fn host_live(retained: &RetainedInstance, stage: &str) {
    assert!(
        !claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap(),
        "{}",
        diagnostic(stage)
    );
    let process = ProcessIdentity::from_pidfd(retained.process.as_fd()).unwrap();
    assert_eq!(process.uid(), OWNER);
}

fn find_native(retained: &RetainedInstance, product: Product) -> Option<NativeProcess> {
    let group =
        std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
    let expected = std::fs::metadata(product.entry()).unwrap();
    let mut matched = Vec::new();
    // The registered Host pid can be bubblewrap, not the native executable.
    for pid in std::fs::read_to_string(group.join("cgroup.procs"))
        .unwrap()
        .lines()
    {
        let pid: i32 = pid.parse().unwrap();
        let Ok(running) = std::fs::metadata(format!("/proc/{pid}/exe")) else {
            continue;
        };
        if running.dev() != expected.dev() || running.ino() != expected.ino() {
            continue;
        }
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
        assert!(
            raw >= 0,
            "actual native process exited while its ELF identity was being retained"
        );
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        let identity = ProcessIdentity::from_pidfd(pidfd.as_fd()).unwrap();
        assert_eq!(identity.uid(), OWNER);
        identity.assert_current().unwrap();
        let current = std::fs::metadata(format!("/proc/{pid}/exe")).unwrap();
        assert_eq!(
            (current.dev(), current.ino()),
            (expected.dev(), expected.ino())
        );
        matched.push(NativeProcess {
            identity,
            pidfd,
            inode: (current.dev(), current.ino()),
        });
    }
    assert!(
        matched.len() <= 1,
        "actual App daemonized into multiple native processes"
    );
    matched.pop()
}

fn check_native_process(retained: &RetainedInstance, native: &NativeProcess) -> Result<(), String> {
    for (name, descriptor) in [
        ("common Host", retained.process.as_fd()),
        ("native App", native.pidfd.as_fd()),
    ] {
        if claw_display_control::identity::pidfd_exited(descriptor)
            .map_err(|error| error.to_string())?
        {
            return Err(format!("{name} exited"));
        }
    }
    let host =
        ProcessIdentity::from_pidfd(retained.process.as_fd()).map_err(|error| error.to_string())?;
    if host.uid() != OWNER {
        return Err("common Host owner changed".into());
    }
    native
        .identity
        .assert_current()
        .map_err(|error| error.to_string())?;
    let running = std::fs::metadata(format!("/proc/{}/exe", native.identity.pid()))
        .map_err(|error| error.to_string())?;
    if (running.dev(), running.ino()) != native.inode {
        return Err("native executable identity changed".into());
    }
    Ok(())
}

fn native_process(retained: &RetainedInstance, native: &NativeProcess, stage: &str) {
    let checked = check_native_process(retained, native);
    assert!(checked.is_ok(), "{}; {checked:?}", diagnostic(stage));
}

fn launcher_output_tail() -> std::io::Result<serde_json::Value> {
    use std::io::{Read, Seek, SeekFrom};

    const LIMIT: u64 = 16384;
    let mut file = std::fs::File::open("/run/gui-fixture/pam-native-product.log")?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(LIMIT)))?;
    let mut bytes = Vec::new();
    file.take(LIMIT).read_to_end(&mut bytes)?;
    Ok(serde_json::json!({
        "untrusted_ordinary_launcher_output": String::from_utf8_lossy(&bytes),
        "log_bytes": length, "tail_truncated": length > LIMIT,
    }))
}

fn native_metadata_syscall_path(process: &NativeProcess) -> Option<serde_json::Value> {
    #[cfg(target_arch = "x86_64")]
    {
        process.identity.assert_current().ok()?;
        let syscall = format!("/proc/{}/syscall", process.identity.pid());
        let before = std::fs::read_to_string(&syscall).ok()?;
        let fields = before.split_whitespace().collect::<Vec<_>>();
        let number = fields.first()?.parse::<i64>().ok()?;
        let (kind, argument) = match number {
            libc::SYS_mkdir => ("mkdir", 1),
            libc::SYS_mkdirat => ("mkdirat", 2),
            libc::SYS_statx => ("statx", 2),
            libc::SYS_newfstatat => ("fstatat", 2),
            _ => return None,
        };
        let address = usize::from_str_radix(fields.get(argument)?.strip_prefix("0x")?, 16).ok()?;
        let mut bytes = [0_u8; 4096];
        let local = libc::iovec { iov_base: bytes.as_mut_ptr().cast(), iov_len: bytes.len() };
        let remote = libc::iovec { iov_base: address as *mut libc::c_void, iov_len: bytes.len() };
        let count = unsafe {
            libc::process_vm_readv(process.identity.pid(), &local, 1, &remote, 1, 0)
        };
        if count < 0 {
            return Some(serde_json::json!({"unavailable": std::io::Error::last_os_error().to_string()}));
        }
        if std::fs::read_to_string(&syscall).ok()? != before
            || process.identity.assert_current().is_err()
        {
            return Some(serde_json::json!({"unavailable": "syscall changed during pathname sampling"}));
        }
        let bytes = &bytes[..count as usize];
        let end = bytes.iter().position(|byte| *byte == 0);
        let length = end.unwrap_or(bytes.len());
        Some(serde_json::json!({
            "kind": kind, "path": String::from_utf8_lossy(&bytes[..length.min(512)]),
            "unterminated": end.is_none(), "truncated": length > 512,
        }))
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = process;
        None
    }
}

fn process_failure_evidence(
    retained: &RetainedInstance,
    process: &NativeProcess,
) -> serde_json::Value {
    fn read(path: &Path) -> serde_json::Value {
        use std::io::Read;
        let result = (|| {
            let mut bytes = Vec::new();
            std::fs::File::open(path)?
                .take(4096)
                .read_to_end(&mut bytes)?;
            Ok::<_, std::io::Error>(String::from_utf8_lossy(&bytes).into_owned())
        })();
        match result {
            Ok(text) => text.into(),
            Err(error) => serde_json::json!({"unavailable": error.to_string()}),
        }
    }

    if let Err(error) = process.identity.assert_current() {
        return serde_json::json!({"identity_unavailable": error.to_string()});
    }
    let root = std::path::PathBuf::from(format!("/proc/{}", process.identity.pid()));
    let syscall_path = native_metadata_syscall_path(process);
    let bounded = |name: &str, limit| {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(root.join(name))?.take(limit).read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    };
    let environment = bounded("environ", 16 * 1024).unwrap_or_default();
    let paths = environment.split(|byte| *byte == 0).filter_map(|entry| {
        let split = entry.iter().position(|byte| *byte == b'=')?;
        let key = std::str::from_utf8(&entry[..split]).ok()?;
        ["HOME", "COS_DATA_DIR", "COS_OWNER_HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME"]
            .contains(&key).then(|| (key, String::from_utf8_lossy(&entry[split + 1..]).into_owned()))
    }).collect::<std::collections::BTreeMap<_, _>>();
    let mountinfo = bounded("mountinfo", 256 * 1024).unwrap_or_default();
    let data_mounts = String::from_utf8_lossy(&mountinfo)
        .lines().filter(|line| {
            line.split_whitespace().nth(4).is_some_and(|target| {
                target == "/run/display-home" || target.starts_with("/run/display-home/")
                    || paths.values().any(|path| path == target)
            })
        }).take(24).map(str::to_string).collect::<Vec<_>>();
    let tasks =
        std::fs::read_dir(root.join("task"))
            .map(|tasks| {
                tasks
        .filter_map(|task| task.ok()).take(32).map(|task| {
            let path = task.path();
            serde_json::json!({
                "tid": task.file_name().to_string_lossy(), "stat": read(&path.join("stat")),
                "status": read(&path.join("status")), "wchan": read(&path.join("wchan")),
                "syscall": read(&path.join("syscall")),
            })
        }).collect::<Vec<_>>()
            })
            .unwrap_or_default();
    let provider_tasks = std::fs::read_dir("/proc/self/task")
        .map(|tasks| tasks.filter_map(Result::ok).take(256).filter_map(|task| {
            let path = task.path();
            let name = std::fs::read_to_string(path.join("comm")).ok()?;
            name.trim().starts_with("claw-native-fs").then(|| serde_json::json!({
                "tid": task.file_name().to_string_lossy(), "name": name.trim(),
                "stat": read(&path.join("stat")), "wchan": read(&path.join("wchan")),
                "syscall": read(&path.join("syscall")), "stack": read(&path.join("stack")),
            }))
        }).take(16).collect::<Vec<_>>())
        .unwrap_or_default();
    let group = std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd()));
    serde_json::json!({
        "cmdline": read(&root.join("cmdline")), "stat": read(&root.join("stat")),
        "task_limit": 32, "tasks": tasks,
        "provider_task_limit": 16, "provider_tasks": provider_tasks,
        "native_fs_io_counters": crate::worker::native_fs::fixture_io_counters(),
        "syscall_path": syscall_path, "path_environment": paths,
        "data_mount_limit": 24, "data_mounts": data_mounts,
        "cgroup_cpu": group.as_ref().map(|path| read(&path.join("cpu.stat"))).ok(),
        "cgroup_memory": group.as_ref().map(|path| read(&path.join("memory.current"))).ok(),
        "cgroup_cpu_limit": group.as_ref().map(|path| read(&path.join("cpu.max"))).ok(),
    })
}

async fn renderer_failure(
    control: &OwnedFd,
    pam: &mut Child,
    retained: &RetainedInstance,
    editor: &NativeProcess,
    session: &str,
    stage: &str,
) -> ! {
    let original = diagnostic(stage);
    let window = claw_display_control::Epoch(retained.instance.0).label();
    let pending = crate::approvals::list_pending_for_owner(Some(OWNER))
        .into_iter()
        .filter(|request| {
            request.verb == "fs.read" && request.session.starts_with(&format!("gui-read-{window}-"))
        })
        .count();
    eprintln!(
        "private actual native renderer failure before cleanup {}",
        serde_json::json!({
            "stage": stage, "readiness_gate_failed": true, "window": window,
            "pending_read_reviews": pending, "witness": observe(),
            "bounded_process_evidence": process_failure_evidence(retained, editor),
        })
    );

    // Keep the broker alive until the ordinary owner CLI can receive its checked outcome.
    let stopped = tokio::time::timeout(
        Duration::from_secs(5),
        crate::clawd::client::request(
            "/run/cos/clawd.sock",
            crate::clawd::wire::Request::new(
                crate::clawd::routes::Command::PermissionRevoke,
                serde_json::json!({"owner_uid": OWNER, "session": session}),
            ),
        ),
    )
    .await;
    let (revoked, error) = match stopped {
        Ok(Ok(reply)) if reply.ok => (true, None),
        Ok(Ok(reply)) => (false, Some(format!("{:?}", reply.error))),
        Ok(Err(error)) => (false, Some(error.to_string())),
        Err(_) => (
            false,
            Some("checked failure retirement deadline".to_string()),
        ),
    };
    let host_exited =
        claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap();
    let editor_exited = claw_display_control::identity::pidfd_exited(editor.pidfd.as_fd()).unwrap();
    let group_deleted = std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd()))
        .unwrap()
        .as_os_str()
        .as_encoded_bytes()
        .ends_with(b" (deleted)");
    let runtime = Path::new("/run/cos/gui").join(&window);
    let mountpoint = runtime.join("home-metadata");
    let runtime_removed = !runtime.exists()
        && !mountpoint.exists()
        && !std::fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .lines()
            .any(|line| line.split_whitespace().nth(4) == mountpoint.to_str());
    let retired = revoked && host_exited && editor_exited && group_deleted && runtime_removed;
    let mut report = serde_json::json!({
        "stage": stage, "checked_retired": retired, "retirement_error": error,
        "host_exited": host_exited, "editor_exited": editor_exited,
        "group_deleted": group_deleted, "runtime_removed": runtime_removed,
    });
    if retired {
        command(control, b"WAIT");
        let ended = status(control, pam, Instant::now() + Duration::from_secs(5));
        report["launcher_status"] = String::from_utf8_lossy(&ended).into_owned().into();
        report["ordinary_output_after_checked_retirement"] = launcher_output_tail()
            .unwrap_or_else(|error| serde_json::json!({"unavailable": error.to_string()}));
        command(control, b"CLSE");
        let deadline = Instant::now() + Duration::from_secs(5);
        let pam_end = loop {
            if let Some(ended) = pam.try_wait().unwrap() {
                break serde_json::json!({"exited": true, "exit_code": ended.code()});
            }
            if Instant::now() >= deadline {
                break serde_json::json!({"exited": false});
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        report["pam"] = pam_end;
        report["retired_witness"] = observe().unwrap_or(serde_json::Value::Null);
    }
    // JSON escaping keeps captured App text inert in the CI log.
    eprintln!("private actual native renderer failure after cleanup {report}");
    panic!("{original}");
}

fn startup_authority(session: &str, parent: &str, product: Product) -> (CapSet, CapSet) {
    crate::paths::RoutedPathContext::for_owner(OWNER, Path::new("/run/display-home").to_path_buf())
        .scope_sync(|| {
            let gui = crate::proc::session_info_by_id(session).unwrap();
            assert_eq!(gui.app_id.as_deref(), Some(product.app));
            let gui = gui.caps.unwrap();
            let parent = crate::proc::session_info_by_id(parent)
                .unwrap()
                .caps
                .unwrap();
            let mut expected_gui = CapSet::from_caps(product.startup_needs());
            let mut expected_parent =
                CapSet::from_caps([Cap::new(Verb::AGENT_INVOKE, Scope::name(product.app))]);
            if product.home_metadata {
                let metadata = Cap::new(Verb::FS_META, Scope::path("/run/display-home/**"));
                expected_gui.insert(metadata.clone());
                expected_parent.insert(metadata);
            }
            assert_eq!(
                gui, expected_gui,
                "GUI startup must not inherit MCP, content, process or AI authority"
            );
            assert_eq!(
                parent, expected_parent,
                "launcher must not borrow a model, Admin or content grant"
            );
            (gui, parent)
        })
}

async fn approve_native_startup(
    pam: &mut Child,
    product: Product,
    parent: &str,
) -> Vec<crate::approvals::Request> {
    let mut approved: Vec<crate::approvals::Request> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    for Cap { verb, scope } in product.startup_needs() {
        let request = loop {
            if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER))
                .into_iter()
                .find(|request| {
                    request.session == parent && request.verb == verb.as_str() && request.scope == scope
                })
            {
                break request;
            }
            assert!(
                pam.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "{}",
                diagnostic("ordinary mandatory native owner review")
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(request.owner_uid, Some(OWNER));
        assert!(request.operation_digest.is_none());
        if let Some(first) = approved.first() {
            assert_eq!(request.session, first.session);
        }
        assert!(
            observe().is_none(),
            "native App ran before mandatory owner decisions"
        );
        approved.push(approve_protected_owner_request(&request.id).await);
    }
    approved
}

fn registered_gui_session<'a>(
    sessions: &'a [crate::proc::SessionInfo],
    parent: &str,
    product: Product,
) -> Option<&'a crate::proc::SessionInfo> {
    let mut matching = sessions.iter().filter(|session| {
        session.app_id.as_deref() == Some(product.app)
            && session.parent.as_deref() == Some(parent)
            && session.group.as_deref() == Some("app")
            && !session.pending_bind
            && session.pid > 1
            && session.start_time_ticks.is_some()
            && session.ended_at.is_none()
    });
    let session = matching.next();
    assert!(
        matching.next().is_none(),
        "ambiguous Root-registered native GUI sessions"
    );
    session
}

fn bound_gui_session(parent: &str, product: Product) -> Option<String> {
    let sessions = crate::proc::try_registry_sessions_for_owner(OWNER).unwrap();
    let session = registered_gui_session(&sessions, parent, product)?;
    crate::proc::process_identity_is_live(session.pid, session.start_time_ticks.unwrap(), OWNER)
        .then(|| session.session_id.clone())
}

pub(super) async fn run(group: &std::fs::File, product: Product) {
    let editor = product.name == "editor";
    let package_owned = Path::new("/run/gui-fixture/native-store-package").exists();
    assert!(!package_owned || product.name == "store");
    let calendar_fixture = product.startup_needs().iter().any(|cap| {
        cap.verb == Verb::DATA_DB_READ && cap.scope == Scope::name("calendar")
    }).then(calendar_popup::Fixture::prepare);
    let mut clipboard_fixture = if product.name == "clipboard" {
        Some(clipboard_history::Fixture::prepare().await)
    } else {
        None
    };
    let store_fixture = product.observation.is_some().then(|| store_catalog::Fixture::prepare(package_owned));
    let home = Path::new("/run/display-home");
    {
        let _owner = crate::clawd::client_identity::FsIdentityGuard::enter(OWNER).unwrap();
        std::fs::write(TARGET, CONTENT).unwrap();
        std::fs::write(
            home.join("native-editor-unselected.txt"),
            b"unselected content stays private\n",
        )
        .unwrap();
    }
    assert!(
        observe().is_none(),
        "stale window observations cannot satisfy a new launch"
    );
    let (control, child_control) = bootstrap::pair().unwrap();
    let mut pam = spawn_pam("gui", "native-product", group, Stdio::from(child_control));
    assert_eq!(
        status(&control, &pam, Instant::now() + Duration::from_secs(30)),
        *b"AUTH"
    );
    command(&control, b"CASE");
    command(
        &control,
        format!("{:02}..", product.case)
            .as_bytes()
            .try_into()
            .unwrap(),
    );
    let child = bootstrap::receive(
        control.as_fd(),
        *b"CHLD",
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(
        kernel::sender(child.credentials).unwrap(),
        ProcessIdentity::child(&pam).unwrap()
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    let launcher = loop {
        let process = ProcessIdentity::from_pidfd(child.descriptor.as_fd()).unwrap();
        if process.uid() == OWNER && process.gid() == OWNER {
            break process;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    let login = claw_display_control::KernelLogin::of(&launcher).unwrap();
    assert_eq!(login.owner_uid, OWNER);
    let display = crate::display_session::registry::Registry::get()
        .unwrap()
        .for_process(&launcher)
        .unwrap();
    assert_eq!(
        crate::display_session::registry::GuiControl::of(&display)
            .unwrap()
            .owner_uid,
        OWNER
    );
    let parent = launch_parent(&launcher, product.case);
    command(&control, b"RUN!");
    let panel_approvals = approve_native_startup(&mut pam, product, &parent).await;
    let metadata = if product.home_metadata {
        let deadline = Instant::now() + Duration::from_secs(20);
        let metadata = loop {
            if let Some(request) = crate::approvals::list_pending_for_owner(Some(OWNER))
                .into_iter()
                .find(|request| {
                    request.verb == "fs.meta"
                        && request.scope == Scope::path("/run/display-home/**")
                        && request.session.starts_with("app-")
                        && request.operation_digest.is_some()
                })
            {
                break request;
            }
            assert!(
                pam.try_wait().unwrap().is_none() && Instant::now() < deadline,
                "{}",
                diagnostic("per-window metadata review")
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_ne!(metadata.session, parent);
        assert!(
            observe().is_none(),
            "actual App code ran before per-window metadata consent"
        );
        assert!(
            !crate::approvals::list_pending_for_owner(Some(OWNER))
                .iter()
                .any(|request| request.verb == "fs.read"),
            "content selection preceded metadata consent"
        );
        approve_native_resource(&metadata.id).await;
        Some(metadata)
    } else {
        None
    };

    let deadline = Instant::now() + Duration::from_secs(25);
    let (session, retained) = loop {
        let session = metadata
            .as_ref()
            .map(|request| request.session.clone())
            .or_else(|| bound_gui_session(&parent, product));
        if let Some(session) = session {
            let bound = crate::paths::RoutedPathContext::for_owner(OWNER, home.to_path_buf())
                .scope_sync(|| {
                    crate::proc::session_info_by_id(&session)
                        .is_some_and(|session| !session.pending_bind && session.pid > 1)
                });
            if bound
                && Path::new("/run/cos/gui")
                    .read_dir()
                    .unwrap()
                    .next()
                    .is_some()
            {
                let retained = retained_instance(&session);
                break (session, retained);
            }
        }
        assert!(
            pam.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "{}",
            diagnostic("common Host process binding")
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let window = claw_display_control::Epoch(retained.instance.0).label();
    let runtime = Path::new("/run/cos/gui").join(&window);
    let mountpoint = runtime.join("home-metadata");
    let process = loop {
        host_live(&retained, "signed native ELF startup/foreground lifetime");
        if let Some(process) = find_native(&retained, product) {
            break process;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            diagnostic("exact signed native ELF execution")
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let original = startup_authority(&session, &parent, product);
    for request in &panel_approvals {
        assert_eq!(request.session, parent);
        assert!(matches!(
            crate::approvals::status_for_owner(&request.id, Some(OWNER)),
            crate::approvals::RequestStatus::Consumed
        ));
    }
    let environment = std::fs::read(format!("/proc/{}/environ", process.identity.pid())).unwrap();
    let value = |key: &str| {
        environment
            .split(|byte| *byte == 0)
            .find_map(|entry| entry.strip_prefix(format!("{key}=").as_bytes()))
    };
    assert_eq!(value("COS_APP_GUI"), Some(b"1".as_slice()));
    assert_eq!(value("ICED_BACKEND"), Some(b"tiny-skia".as_slice()));
    assert_eq!(value("COS_COMMAND"), Some(product.selector.as_bytes()));
    assert_eq!(value("COS_SESSION"), Some(session.as_bytes()));
    assert_eq!(
        value("COS_OWNER_HOME"),
        Some(b"/run/display-home".as_slice())
    );
    assert_eq!(
        value("COS_APP_RESOURCE_DIR"),
        Some(
            product
                .directory()
                .join("resources")
                .as_os_str()
                .as_encoded_bytes()
        )
    );
    assert!(value("XDG_DATA_DIRS")
        .unwrap()
        .starts_with(format!("{}/resources/usr/share:", product.directory().display()).as_bytes()));
    assert_ne!(value("HOME"), Some(b"/run/display-home".as_slice()));
    let data_source = home.join(".local/share/cos/apps").join(product.app);
    let data_target = if product.home_metadata {
        Path::new("/run/cos/app-data")
    } else {
        data_source.as_path()
    };
    assert_eq!(value("COS_DATA_DIR"), Some(data_target.as_os_str().as_encoded_bytes()));
    assert_eq!(value("HOME"), value("COS_DATA_DIR"));
    let source = std::fs::metadata(&data_source).unwrap();
    let exposed = std::fs::metadata(
        PathBuf::from(format!("/proc/{}/root", process.identity.pid()))
            .join(data_target.strip_prefix("/").unwrap()),
    ).unwrap();
    assert_eq!((exposed.dev(), exposed.ino()), (source.dev(), source.ino()),
        "namespace-only private data projection changed its persistent backing");
    assert_eq!(exposed.uid(), OWNER);
    assert_eq!(exposed.mode() & 0o077, 0);
    assert_ne!(value("COS_MCP_SERVER"), Some(b"1".as_slice()));
    if product.ungranted.is_empty() {
        assert!(value("COS_APP_OPTIONAL_NEEDS_UNGRANTED_JSON").is_none());
    } else {
        let ungranted: Vec<usize> =
            serde_json::from_slice(value("COS_APP_OPTIONAL_NEEDS_UNGRANTED_JSON").unwrap())
                .unwrap();
        assert_eq!(ungranted, product.ungranted);
    }
    let args = if editor {
        format!("[\"{TARGET}\"]")
    } else {
        "[]".into()
    };
    assert_eq!(value("COS_ARGS_JSON"), Some(args.as_bytes()));

    let mut read = None;
    let startup = Instant::now();
    let mut startup_samples = 0;
    let mut next_startup_sample = startup;
    let before = loop {
        if let Err(error) = check_native_process(&retained, &process) {
            renderer_failure(
                &control,
                &mut pam,
                &retained,
                &process,
                &session,
                &format!("actual native startup stopped before its real window: {error}"),
            )
            .await;
        }
        if product.name == "files" && startup_samples < 8 && Instant::now() >= next_startup_sample {
            eprintln!("private actual Files startup sample {}", serde_json::json!({
                "instance": window, "elapsed_ms": startup.elapsed().as_millis(),
                "io": crate::worker::native_fs::fixture_io_counters(),
                "syscall_path": native_metadata_syscall_path(&process),
            }));
            startup_samples += 1;
            next_startup_sample = Instant::now() + Duration::from_secs(3);
        }
        if editor && read.is_none() {
            read = crate::approvals::list_pending_for_owner(Some(OWNER))
                .into_iter()
                .find(|request| {
                    request.verb == "fs.read"
                        && request.session.starts_with(&format!("gui-read-{window}-"))
                });
        }
        if let Some(record) = observe().filter(|record| {
            (if product.interactive_panel {
                buffered_palette(record, &window)
            } else if product.name == "desktop-widgets" {
                buffered_widget_panel(record, &window)
            } else if product.panel {
                buffered_panel(record, &window, product.desktop_id)
            } else {
                buffered_window(record, &window) && record["app_id"] == product.desktop_id
            }) && (!editor || read.is_some())
        }) {
            break record;
        }
        if pam.try_wait().unwrap().is_some() || Instant::now() >= deadline {
            renderer_failure(
                &control,
                &mut pam,
                &retained,
                &process,
                &session,
                if product.panel {
                    "actual Layer Shell configure/SHM icon pixels under the live ui.panel lease"
                } else {
                    "actual xdg_toplevel configure/SHM frame and declared startup behavior"
                },
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    if product.name == "files" {
        let evidence = process_failure_evidence(&retained, &process);
        eprintln!("private actual Files first-frame profile {}", serde_json::json!({
            "instance": window, "elapsed_ms": startup.elapsed().as_millis(),
            "startup_samples": startup_samples, "io": evidence["native_fs_io_counters"],
            "path_environment": evidence["path_environment"],
            "data_mounts": evidence["data_mounts"], "cgroup_cpu": evidence["cgroup_cpu"],
        }));
    }
    if editor {
        assert!(
            !loaded(&before),
            "startup file was loaded before independent content consent"
        );
    } else {
        assert!(!crate::approvals::list_pending_for_owner(Some(OWNER))
            .iter()
            .any(|request| request.verb == "fs.read"));
    }
    if let Some(metadata) = &metadata {
        assert!(matches!(
            crate::approvals::status_for_owner(&metadata.id, Some(OWNER)),
            crate::approvals::RequestStatus::Consumed
        ));
    }
    if let Some(read) = &read {
        let metadata = metadata.as_ref().unwrap();
        assert_eq!(read.owner_uid, Some(OWNER));
        assert_eq!(read.scope, Scope::path("/run/display-home/**"));
        assert_ne!(read.session, metadata.session);
        assert!(
            read.operation_digest.is_some() && read.operation_digest != metadata.operation_digest
        );
        println!(
            "private actual Editor window ready; separate startup-file fs.read remains pending"
        );
        approve_native_resource(&read.id).await;
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            native_process(
                &retained,
                &process,
                "actual native file load after independent consent",
            );
            if observe().is_some_and(|record| {
                buffered_window(&record, &window)
                    && loaded(&record)
                    && record["buffer_commits"].as_u64().unwrap()
                        > before["buffer_commits"].as_u64().unwrap()
            }) {
                break;
            }
            if Instant::now() >= deadline {
                renderer_failure(
                    &control,
                    &mut pam,
                    &retained,
                    &process,
                    &session,
                    "actual loaded-tab title and a newly rendered native frame",
                )
                .await;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(matches!(
            crate::approvals::status_for_owner(&read.id, Some(OWNER)),
            crate::approvals::RequestStatus::Consumed
        ));
    }
    assert_eq!(startup_authority(&session, &parent, product), original);
    let saved = if editor {
        assert!(
            read.is_some(),
            "Editor Save As must follow its accepted startup read"
        );
        Some(editor_save_as::run(&control, &mut pam, &retained, &process, &session, &parent).await)
    } else {
        None
    };
    let pty = if product.name == "terminal" {
        let proof = terminal_pty::run(
            &control, &mut pam, &retained, &process, &session, deadline, &before,
        )
        .await;
        assert_eq!(startup_authority(&session, &parent, product), original);
        Some(proof)
    } else {
        None
    };
    if let Some(fixture) = &calendar_fixture {
        if product.name == "desktop-widgets" {
            if let Err(error) = fixture.consume_widgets(&retained, &process, &session, deadline, &before).await {
                renderer_failure(&control, &mut pam, &retained, &process, &session, &error).await;
            }
        } else {
            calendar_popup::run(&control, &mut pam, &retained, &process, &session, fixture).await;
        }
        assert_eq!(startup_authority(&session, &parent, product), original);
    }
    if let Some(fixture) = &mut clipboard_fixture {
        fixture.consume(&control, &mut pam, &retained, &process, &session).await;
        assert_eq!(startup_authority(&session, &parent, product), original);
    }
    let mut package_review = None;
    if let Some(fixture) = &store_fixture {
        if let Err(error) = fixture.consume(&retained, &process, &session, deadline, &before).await {
            renderer_failure(&control, &mut pam, &retained, &process, &session, &error).await;
        }
        let searched = match fixture.input_frame(&retained, &process, &before, (1, "text", "prive")).await {
            Ok(frame) => frame,
            Err(error) => renderer_failure(&control, &mut pam, &retained, &process, &session, &error).await,
        };
        if package_owned {
            let mut frame = searched;
            for input in [
                (2, "pointer", "500,80"), (3, "pointer", "540,230"),
                (4, "pointer", "600,350"),
            ] {
                frame = match fixture.input_frame(&retained, &process, &frame, input).await {
                    Ok(frame) => frame,
                    Err(error) => renderer_failure(&control, &mut pam, &retained, &process, &session, &error).await,
                };
            }
            package_review = Some(match fixture.package_review(&retained, &process, &session).await {
                Ok(review) => review,
                Err(error) => renderer_failure(&control, &mut pam, &retained, &process, &session, &error).await,
            });
        }
        assert_eq!(startup_authority(&session, &parent, product), original);
    }
    assert_eq!(
        std::fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .lines()
            .any(|line| line.split_whitespace().nth(4) == mountpoint.to_str()),
        product.home_metadata,
        "only a metadata-authorized App may have the controlled Home mount"
    );
    let group_path =
        std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd())).unwrap();
    let processes = std::fs::read_to_string(group_path.join("cgroup.procs"))
        .unwrap()
        .lines()
        .map(|pid| {
            let raw =
                unsafe { libc::syscall(libc::SYS_pidfd_open, pid.parse::<i32>().unwrap(), 0_u32) };
            assert!(raw >= 0);
            unsafe { OwnedFd::from_raw_fd(raw as i32) }
        })
        .collect::<Vec<_>>();
    assert!(!processes.is_empty());
    let generation = crate::approvals::generations::current(Some(OWNER), &session).unwrap();
    let mut package_applied = None;
    if let (Some(fixture), Some(review)) = (&store_fixture, &package_review) {
        let approved = approve_protected_owner_request(&review.id).await;
        assert_eq!(approved.operation_digest, review.operation_digest);
        assert_eq!(startup_authority(&session, &parent, product), original);
        package_applied = Some(match fixture.self_handoff(&retained, &process, &session).await {
            Ok(record) => record,
            Err(error) => renderer_failure(&control, &mut pam, &retained, &process, &session, &error).await,
        });
    }
    if package_applied.is_none() {
        let revoked = crate::clawd::client::request(
            "/run/cos/clawd.sock",
            crate::clawd::wire::Request::new(
                crate::clawd::routes::Command::PermissionRevoke,
                serde_json::json!({
                    "owner_uid": OWNER,
                    "session": read.as_ref().map(|request| request.session.as_str())
                        .unwrap_or(&session),
                }),
            ),
        )
        .await
        .unwrap();
        assert!(
            revoked.ok,
            "actual native resource retirement failed: {:?}",
            revoked.error
        );
        let after = crate::approvals::generations::current(Some(OWNER), &session).unwrap();
        if editor {
            assert_eq!(after, generation);
        } else {
            assert!(after > generation);
        }
    }
    assert!(claw_display_control::identity::pidfd_exited(retained.process.as_fd()).unwrap());
    assert!(claw_display_control::identity::pidfd_exited(process.pidfd.as_fd()).unwrap());
    for process in processes {
        assert!(claw_display_control::identity::pidfd_exited(process.as_fd()).unwrap());
    }
    if let Some(pty) = &pty {
        pty.verify_after_retirement();
    }
    if let Some(fixture) = &calendar_fixture {
        fixture.verify_after_retirement();
    }
    if let Some(fixture) = &store_fixture {
        fixture.verify_after_retirement(package_applied.is_some());
    }
    if let Some(review) = &package_review {
        let status = crate::approvals::status_for_owner(&review.id, Some(OWNER));
        if package_applied.is_some() {
            assert!(matches!(status, crate::approvals::RequestStatus::Consumed));
        } else {
            assert!(matches!(status, crate::approvals::RequestStatus::Denied));
        }
    }
    assert!(
        std::fs::read_link(format!("/proc/self/fd/{}", retained.group.as_raw_fd()))
            .unwrap()
            .as_os_str()
            .as_encoded_bytes()
            .ends_with(b" (deleted)")
    );
    assert!(
        !runtime.exists() && !mountpoint.exists(),
        "actual native runtime/provider cleanup was not acknowledged"
    );
    assert!(!std::fs::read_to_string("/proc/self/mountinfo")
        .unwrap()
        .lines()
        .any(|line| line.split_whitespace().nth(4) == mountpoint.to_str()));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if observe().is_some_and(|record| {
            record["instance"] == window
                && record["closed"] == true
                && record["toplevels"] == 0
                && (!product.panel
                    || (record["schema"] == "claw.private-native-panel/v1"
                        && record["role"] == "zwlr_layer_surface_v1"
                        && record["layer_surfaces"] == 0
                        && record["popup"]["open"] == false
                        && (product.name == "desktop-widgets" || product.interactive_panel
                            || record["popup"]["closed"] == true)))
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual native Wayland window did not close"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(std::fs::read(TARGET).unwrap(), CONTENT);
    assert_eq!(
        std::fs::read(home.join("native-editor-unselected.txt")).unwrap(),
        b"unselected content stays private\n"
    );
    if let Some(saved) = &saved {
        saved.verify_after_retirement();
    }
    command(&control, b"WAIT");
    assert_eq!(
        status(&control, &pam, Instant::now() + Duration::from_secs(30)),
        *b"FAIL"
    );
    command(&control, b"CLSE");
    assert!(pam.wait().unwrap().success());
    let login_group = std::fs::read_link(format!("/proc/self/fd/{}", group.as_raw_fd())).unwrap();
    assert!(
        std::fs::read_to_string(login_group.parent().unwrap().join("cgroup.events"))
            .unwrap()
            .lines()
            .any(|line| line == "populated 0")
    );
    if let Some(fixture) = clipboard_fixture {
        fixture.stop().await;
    }
    println!(
        "private actual {}: real signed ELF/resources, native window{} and checked process/runtime/PAM retirement passed; {}",
        product.name,
        if editor {
            "/file load"
        } else if product.home_metadata {
            "/metadata-only Home"
        } else if product.panel {
            "/Layer Shell panel with independent protected owner approvals"
        } else if package_applied.is_some() {
            "/retained original-UI package self-handoff"
        } else if product.observation.is_some() {
            "/protected ordinary package observation"
        } else {
            "/empty-authority private PTY"
        },
        if editor {
            "UI Save As and separately pending overwrite verified"
        } else if pty.is_some() {
            "typed shell command, real PTY/shell/child identities and descendant retirement verified"
        } else if product.interactive_panel {
            "original managed interactive palette pixels verified; catalog/search, typed launch and backend resource admission NOT asserted"
        } else if product.name == "desktop-widgets" {
            "real owner-scoped Calendar/task/resource reads under independent exact grants verified; reply decoding and exact painted card text NOT asserted"
        } else if product.name == "clipboard" {
            "real private OS CopyQ history read/delete/confirmed-clear, authenticated Root headless selection Restore with plain-text/HTML transfers and original popup open/close verified; physical display and exact painted history text NOT asserted"
        } else if product.panel {
            "original popup pixels/open/close and authenticated same-owner real SDK/Calendar reader success verified; exact returned/painted event text NOT inspected"
        } else if package_applied.is_some() {
            "original Store UI exact Critical consent, confirmed own-payload removal, actual APT child zero and ordinary-owner read-only recovery outside the App verified"
        } else if product.observation.is_some() {
            "real controlled catalog/inventory/update SDK requests verified without raw system state or Critical startup authority; entry pixels and package UI mutations NOT inspected"
        } else {
            "additional UI mutations not exercised"
        },
    );
}

#[test]
fn native_editor_readiness_rejects_titles_without_real_configured_pixels() {
    let mut record = serde_json::json!({
        "schema": "claw.private-native-window/v1", "instance": "window",
        "toplevels": 1, "configure_acked": true, "closed": false,
        "buffer_commits": 1, "title": "native-editor-startup.txt - Text Editor",
    });
    assert!(!buffered_window(&record, "window"));
    record["raster"] = serde_json::json!({"width": 800, "height": 600, "nonuniform_pixels": false});
    assert!(!buffered_window(&record, "window"));
    record["raster"]["nonuniform_pixels"] = true.into();
    record["render_trace"] = serde_json::json!({
        "top_pre_commits": 3, "top_pending_buffers": 3,
        "top_frame_callbacks_sent": 3, "configure_acks": 1,
    });
    record["buffer_commits"] = 0.into();
    assert!(
        !buffered_window(&record, "window"),
        "commit/callback diagnostics cannot substitute for an actual observed pixel buffer"
    );
    record["buffer_commits"] = 1.into();
    assert!(buffered_window(&record, "window") && loaded(&record));
    assert!(!buffered_window(&record, "another-window"));
    record["configure_acked"] = false.into();
    assert!(!buffered_window(&record, "window"));
}

#[test]
#[ignore = "clean-GHA-only: actual Editor ELF, authenticated private PAM/display, FUSE and cgroup v2"]
fn private_native_editor() {
    private_gui_fixture_selected(false, false, false, Some(EDITOR));
}

#[test]
#[ignore = "clean-GHA-only: actual Files ELF, authenticated private PAM/display, FUSE and cgroup v2"]
fn private_native_files() {
    private_gui_fixture_selected(false, false, false, Some(FILES));
}

#[test]
#[ignore = "clean-GHA-only: actual Terminal ELF/PTY, authenticated private PAM/display and cgroup v2"]
fn private_native_terminal() {
    private_gui_fixture_selected(false, false, false, Some(TERMINAL));
}

#[test]
#[ignore = "clean-GHA-only: actual panel ELF, authenticated private PAM/display and cgroup v2"]
fn private_native_panel() {
    let path = Path::new("/run/gui-fixture/native-panel-product");
    let metadata = std::fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file() && metadata.uid() == 0 && metadata.nlink() == 1
        && metadata.mode() & 0o077 == 0 && metadata.len() <= 32);
    let product = match std::fs::read(path).unwrap().as_slice() {
        b"calendar" => CALENDAR,
        b"desktop-widgets" => WIDGETS,
        b"clipboard" => CLIPBOARD,
        b"launcher" => LAUNCHER,
        _ => panic!("unknown protected native panel fixture selection"),
    };
    private_gui_fixture_selected(false, false, false, Some(product));
}

#[test]
#[ignore = "clean-GHA-only: actual Store ELF, authenticated private PAM/display/catalog and cgroup v2"]
fn private_native_store() {
    let package_owned = match std::fs::read("/run/gui-fixture/native-store-package") {
        Ok(value) => {
            assert_eq!(value, b"private-store-package-v1");
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => panic!("read private Store package selector: {error}"),
    };
    private_gui_fixture_selected(false, false, package_owned, Some(STORE));
}

#[test]
fn native_calendar_readiness_requires_typed_configured_panel_pixels_not_an_xdg_window() {
    let record = serde_json::json!({
        "schema": "claw.private-native-panel/v1", "instance": "instance",
        "role": "zwlr_layer_surface_v1", "namespace": CALENDAR.desktop_id, "layer": "Top",
        "layer_surfaces": 1, "toplevels": 0, "keyboard_interactivity": "None",
        "configure_acked": true, "configure_size": [40, 40], "closed": false,
        "buffer_commits": 1, "raster": {"width": 40, "height": 40, "nonuniform_pixels": true},
    });
    assert!(buffered_panel(&record, "instance", CALENDAR.desktop_id));
    assert!(!buffered_window(&record, "instance"));
    assert!(!buffered_panel(&record, "foreign", CALENDAR.desktop_id));
    let mut rail = record.clone();
    rail["namespace"] = WIDGETS.desktop_id.into();
    rail["configure_size"] = serde_json::json!([320, 620]);
    rail["raster"] = serde_json::json!({"width":320,"height":620,"nonuniform_pixels":true});
    assert!(buffered_widget_panel(&rail, "instance"));
    assert!(!buffered_widget_panel(&rail, "foreign"));
    let mut tall_calendar = rail.clone();
    tall_calendar["namespace"] = CALENDAR.desktop_id.into();
    assert!(!buffered_panel(&tall_calendar, "instance", CALENDAR.desktop_id));
    rail["configure_size"] = serde_json::json!([40, 620]);
    rail["raster"]["width"] = 40.into();
    assert!(!buffered_widget_panel(&rail, "instance"));
    let mut palette = record.clone();
    palette["namespace"] = "launcher".into();
    palette["keyboard_interactivity"] = "Exclusive".into();
    palette["configure_size"] = serde_json::json!([840, 480]);
    palette["raster"] = serde_json::json!({"width":840,"height":480,"nonuniform_pixels":true});
    assert!(buffered_palette(&palette, "instance"));
    assert!(!buffered_palette(&palette, "foreign"));
    assert!(!buffered_widget_panel(&palette, "instance"));
    assert!(!buffered_panel(&palette, "instance", "launcher"));
    palette["keyboard_interactivity"] = "None".into();
    assert!(!buffered_palette(&palette, "instance"));
    for (field, invalid) in [
        ("schema", serde_json::json!("claw.private-native-window/v1")),
        ("role", serde_json::json!("xdg_toplevel")),
        ("namespace", serde_json::json!("another.panel")),
        ("toplevels", serde_json::json!(1)),
        ("layer_surfaces", serde_json::json!(0)),
        ("configure_acked", serde_json::json!(false)),
        ("configure_size", serde_json::json!([48, 40])),
        ("buffer_commits", serde_json::json!(0)),
        ("keyboard_interactivity", serde_json::json!("Exclusive")),
        (
            "raster",
            serde_json::json!({"width": 40, "height": 40, "nonuniform_pixels": false}),
        ),
        ("closed", serde_json::json!(true)),
    ] {
        let mut invalid_record = record.clone();
        invalid_record[field] = invalid;
        assert!(
            !buffered_panel(&invalid_record, "instance", CALENDAR.desktop_id),
            "{field}"
        );
    }
    let mut ordinary = record;
    ordinary["schema"] = "claw.private-native-window/v1".into();
    ordinary["toplevels"] = 1.into();
    assert!(
        !buffered_window(&ordinary, "instance"),
        "small panels must not lower ordinary-window bounds"
    );
}

#[test]
fn native_terminal_session_selection_refuses_unbound_foreign_and_mcp_rows() {
    let row: crate::proc::SessionInfo = serde_json::from_value(serde_json::json!({
        "session_id": "root-registered-session", "pid": 42, "command": [],
        "started_at": "", "stdout_path": "", "stderr_path": "",
        "group": "app", "parent": "parent", "app_id": "cosmic-term",
        "pending_bind": false, "start_time_ticks": 1,
    }))
    .unwrap();
    assert!(registered_gui_session(&[], "parent", TERMINAL).is_none());
    let mut foreign = row.clone();
    foreign.parent = Some("foreign".into());
    let mut other_app = row.clone();
    other_app.app_id = Some("cosmic-files".into());
    let mut mcp = row.clone();
    mcp.group = Some("app-mcp".into());
    let mut pending = row.clone();
    pending.pending_bind = true;
    let mut ended = row.clone();
    ended.ended_at = Some("ended".into());
    let mut unpinned = row.clone();
    unpinned.start_time_ticks = None;
    let mut rows = vec![foreign, other_app, mcp, pending, ended, unpinned];
    assert!(registered_gui_session(&rows, "parent", TERMINAL).is_none());
    rows.push(row);
    assert_eq!(
        registered_gui_session(&rows, "parent", TERMINAL)
            .unwrap()
            .session_id,
        "root-registered-session"
    );
}

#[test]
#[should_panic(expected = "ambiguous Root-registered native GUI sessions")]
fn native_terminal_session_selection_refuses_ambiguous_root_rows() {
    let row: crate::proc::SessionInfo = serde_json::from_value(serde_json::json!({
        "session_id": "root-registered-session", "pid": 42, "command": [],
        "started_at": "", "stdout_path": "", "stderr_path": "",
        "group": "app", "parent": "parent", "app_id": "cosmic-term",
        "pending_bind": false, "start_time_ticks": 1,
    }))
    .unwrap();
    registered_gui_session(&[row.clone(), row], "parent", TERMINAL);
}
