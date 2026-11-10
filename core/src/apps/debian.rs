//! App delivery inspection, never authorization to execute package content.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path};
use std::process::{Command, Stdio};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::store::AptPackage;

const MAX_FILES: usize = 20000;
const MAX_CONTROL: u64 = 4 * 1024 * 1024;
const MAX_DATA: u64 = 2 * 1024 * 1024 * 1024;

pub(crate) fn file_binding(path: &Path) -> Result<(u64, String), String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 1024 * 1024 * 1024
    {
        return Err("Debian delivery must be a bounded, protected regular file".into());
    }
    let mut input = file.take(1024 * 1024 * 1024 + 1);
    let mut hash = Sha256::new();
    let mut buffer = [0; 32768];
    let mut size = 0;
    loop {
        let count = input.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        size += count as u64;
    }
    if size != metadata.len() {
        return Err("Debian delivery changed while its bytes were inspected".into());
    }
    Ok((size, hex::encode(hash.finalize())))
}

fn canonical_path(path: &Path) -> Result<std::path::PathBuf, String> {
    let mut result = std::path::PathBuf::new();
    for (index, component) in path.components().enumerate() {
        match component {
            Component::CurDir if index == 0 => {}
            Component::Normal(value) => result.push(value),
            _ => return Err("Debian delivery has a non-relative or traversing path".into()),
        }
    }
    let text = result
        .to_str()
        .ok_or("Debian delivery paths must be UTF-8")?;
    if text.len() > 4096 || text.contains('\\') || text.chars().any(char::is_control) {
        return Err("Debian delivery has an invalid path".into());
    }
    Ok(result)
}

fn data_path_allowed(path: &Path, directory: bool) -> bool {
    let text = path.to_string_lossy();
    if directory && matches!(text.as_ref(), "" | "usr" | "usr/lib") {
        return true;
    }
    if !(text == "usr/bin"
        || text.starts_with("usr/bin/")
        || text == "usr/lib/cos"
        || text.starts_with("usr/lib/cos/")
        || text == "usr/share"
        || [
            "applications",
            "icons",
            "metainfo",
            "thumbnailers",
            "cosmic",
            "doc",
            "licenses",
            "claw",
            "claw-apps",
        ]
        .iter()
        .any(|name| path.starts_with(Path::new("usr/share").join(name))))
    {
        return false;
    }
    if path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some(
                "systemd"
                    | "dbus-1"
                    | "polkit-1"
                    | "pam.d"
                    | "pam-configs"
                    | "udev"
                    | "sysusers.d"
                    | "tmpfiles.d"
                    | "autostart"
            )
        )
    }) {
        return false;
    }
    ![
        "usr/lib/cos/bin",
        "usr/lib/cos/trust",
        "usr/lib/cos/apt",
        "usr/lib/cos/python/claw_os_sdk",
        "usr/lib/cos/python/cos_runtime",
        "usr/lib/cos/browser-native-host.py",
        "usr/lib/cos/app-apt-metadata.py",
        "usr/lib/cos/claw-mail-ai-host",
    ]
    .iter()
    .any(|reserved| path == Path::new(reserved) || path.starts_with(reserved))
}

fn relative_link(path: &Path, target: &Path, hard: bool) -> Result<std::path::PathBuf, String> {
    let mut resolved = if hard {
        std::path::PathBuf::new()
    } else {
        path.parent().ok_or("link has no parent")?.to_path_buf()
    };
    for component in target.components() {
        match component {
            Component::Normal(value) => resolved.push(value),
            Component::CurDir => {}
            Component::ParentDir if resolved.pop() => {}
            _ => return Err("App delivery link escapes its owned /usr resources".into()),
        }
    }
    if !data_path_allowed(&resolved, false) {
        return Err("App delivery link selects an unsupported or protected resource".into());
    }
    Ok(resolved)
}

fn tar_entries(
    package: &Path,
    control: bool,
    destination: Option<&Path>,
    budget: u64,
) -> Result<(BTreeMap<String, Vec<u8>>, u64), String> {
    let mut command = Command::new("/usr/bin/dpkg-deb");
    command
        .args([if control {
            "--ctrl-tarfile"
        } else {
            "--fsys-tarfile"
        }])
        .arg(package)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let pid = child.id();
    let output = child
        .stdout
        .take()
        .ok_or("Debian archive stream is unavailable")?;
    let root = destination.map(Path::to_path_buf);
    let reader = std::thread::spawn(move || {
        let ceiling = if control {
            MAX_CONTROL
        } else {
            MAX_DATA.min(budget)
        };
        let mut archive = tar::Archive::new(output.take(ceiling + 1));
        let mut paths = BTreeSet::new();
        let mut metadata = BTreeMap::new();
        let mut links = BTreeSet::new();
        let mut regular = BTreeSet::new();
        let mut total = 0_u64;
        for entry in archive.entries().map_err(|error| error.to_string())? {
            let mut entry = entry.map_err(|error| error.to_string())?;
            let path = canonical_path(&entry.path().map_err(|error| error.to_string())?)?;
            let header = entry.header();
            let kind = header.entry_type();
            let mode = header.mode().map_err(|error| error.to_string())?;
            if paths.len() >= MAX_FILES
                || !paths.insert(path.clone())
                || !(kind.is_dir()
                    || kind.is_file()
                    || !control && (kind.is_symlink() || kind.is_hard_link()))
                || mode & 0o7000 != 0
                || !kind.is_symlink() && mode & 0o022 != 0
                || path
                    .ancestors()
                    .skip(1)
                    .any(|parent| links.contains(parent))
                || header.uid().map_err(|error| error.to_string())? != 0
                || header.gid().map_err(|error| error.to_string())? != 0
            {
                return Err("App Debian payload contains links, privilege, writable files or duplicate paths".into());
            }
            if let Some(extensions) = entry.pax_extensions().map_err(|error| error.to_string())? {
                for extension in extensions {
                    let extension = extension.map_err(|error| error.to_string())?;
                    let key = extension.key().map_err(|error| error.to_string())?;
                    if !matches!(key, "path" | "linkpath") {
                        return Err(
                            "App Debian payload cannot override authority or unindexed metadata"
                                .into(),
                        );
                    }
                }
            }
            total = total
                .checked_add(entry.size())
                .ok_or("Debian expansion overflow")?;
            if total > ceiling {
                return Err("Debian payload exceeds its bounded expansion limit".into());
            }
            if control {
                if kind.is_dir() && path.as_os_str().is_empty() {
                    continue;
                }
                if !kind.is_file() || !matches!(path.to_str(), Some("control" | "md5sums")) {
                    return Err(
                        "Apps cannot supply Debian hooks, triggers or authority control files"
                            .into(),
                    );
                }
                let mut bytes = Vec::new();
                entry
                    .read_to_end(&mut bytes)
                    .map_err(|error| error.to_string())?;
                metadata.insert(path.to_string_lossy().into_owned(), bytes);
            } else {
                if !data_path_allowed(&path, kind.is_dir()) {
                    return Err(format!("App Debian payload cannot own {}", path.display()));
                }
                if kind.is_symlink() || kind.is_hard_link() {
                    if path.starts_with("usr/lib/cos/apps") {
                        return Err("Signed App trees cannot contain links".into());
                    }
                    let target = entry
                        .link_name()
                        .map_err(|error| error.to_string())?
                        .ok_or("App delivery link has no target")?
                        .into_owned();
                    let target = relative_link(&path, &target, kind.is_hard_link())?;
                    if kind.is_hard_link() && !regular.contains(&target) {
                        return Err("App delivery hardlink has no owned regular target".into());
                    }
                    if kind.is_symlink() {
                        links.insert(path.clone());
                    }
                } else if kind.is_file() {
                    regular.insert(path.clone());
                }
                if !entry
                    .unpack_in(root.as_ref().ok_or("missing App inspection root")?)
                    .map_err(|error| error.to_string())?
                {
                    return Err("App Debian payload escapes its inspection root".into());
                }
            }
        }
        if archive.into_inner().limit() == 0 {
            return Err("Debian archive stream exceeds its byte limit".into());
        }
        Ok((metadata, total))
    });
    let (status, timed_out) =
        crate::worker::exec::wait_bounded(&mut child, Some(Duration::from_secs(120)), || unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        })?;
    let result = reader
        .join()
        .map_err(|_| "Debian inspection reader failed")?;
    if timed_out || !status.success() {
        return Err("Debian archive inspection did not complete successfully".into());
    }
    result
}

pub(crate) fn inspect(
    path: &Path,
    expected: &AptPackage,
    destination: &Path,
    budget: u64,
) -> Result<u64, String> {
    if file_binding(path)? != (expected.size, expected.sha256.clone()) {
        return Err("Debian download differs from its authenticated package inventory".into());
    }
    let (control, _) = tar_entries(path, true, None, MAX_CONTROL)?;
    let text = std::str::from_utf8(control.get("control").ok_or("missing Debian control")?)
        .map_err(|_| "Debian control is not UTF-8")?;
    let fields = control_fields(text)?;
    if fields.get("Package") != Some(&expected.package)
        || fields.get("Version") != Some(&expected.version)
        || fields.get("Architecture") != Some(&expected.architecture)
    {
        return Err("Debian control differs from its signed delivery identity".into());
    }
    if fields
        .get("Conflicts")
        .is_some_and(|value| !value.is_empty())
        || fields
            .get("Pre-Depends")
            .is_some_and(|value| !value.is_empty())
        || fields
            .get("Provides")
            .is_some_and(|value| value.split(',').any(|item| !app_provide(item.trim())))
    {
        return Err(
            "App packages cannot claim OS authority, conflicts or pre-install effects".into(),
        );
    }
    for name in ["Breaks", "Replaces"] {
        if fields.get(name).is_some_and(|value| {
            value.split(',').any(|item| {
                !matches!(
                    item.trim(),
                    "claw-os-agent (<< 1:0.2.1)" | "claw-os-desktop (<< 1:0.2.1)"
                )
            })
        }) {
            return Err("App package ownership transfer exceeds the bounded OS cutover".into());
        }
    }
    std::fs::create_dir(destination).map_err(|error| error.to_string())?;
    let (_, expanded) = tar_entries(path, false, Some(destination), budget)?;
    Ok(expanded)
}

fn app_provide(value: &str) -> bool {
    value == "claw-app-support-v1"
        || value
            .strip_prefix("claw-app-python-")
            .and_then(|value| value.strip_suffix("-v1"))
            .is_some_and(|library| {
                !library.is_empty()
                    && library.as_bytes()[0].is_ascii_lowercase()
                    && library.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            })
}

fn control_fields(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut previous = None;
    for line in text.lines() {
        if line.starts_with([' ', '\t']) {
            let value = fields
                .get_mut(previous.as_deref().ok_or("invalid control continuation")?)
                .ok_or("missing control continuation field")?;
            value.push(' ');
            value.push_str(line.trim());
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once(':').ok_or("invalid Debian control field")?;
        if fields.insert(key.into(), value.trim().into()).is_some() {
            return Err("duplicate Debian control field".into());
        }
        previous = Some(key.to_string());
    }
    Ok(fields)
}

fn shell_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_/@%+=:,.-".contains(&byte))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

pub(crate) fn validate_exports(
    root: &Path,
    apps: &BTreeMap<String, super::App>,
) -> Result<(), String> {
    let mut exports = BTreeMap::new();
    for app in apps.values() {
        let Some(desktop) = &app.manifest.desktop else {
            continue;
        };
        for name in
            std::iter::once(None).chain(desktop.entries.keys().map(|name| Some(name.as_str())))
        {
            let gui = app.manifest.gui_entry(name)?;
            let program = Path::new(gui.entry)
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or("GUI entry has no canonical exported program")?;
            let body = format!(
                "#!/bin/sh\nexec /usr/local/bin/cos app {} {} \"$@\"\n",
                shell_word(&app.manifest.id),
                shell_word(&gui.cli_selector())
            );
            if exports.insert(program.to_string(), body).is_some() {
                return Err("App package exports an ambiguous program".into());
            }
        }
    }
    let binaries = root.join("usr/bin");
    if binaries.exists() {
        for entry in std::fs::read_dir(binaries).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "invalid App export name")?;
            let metadata =
                std::fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
            if !metadata.is_file()
                || metadata.len() > 8192
                || exports.get(&name).map(String::as_bytes)
                    != Some(
                        std::fs::read(entry.path())
                            .map_err(|error| error.to_string())?
                            .as_slice(),
                    )
            {
                return Err("App executable export bypasses its declared common Host".into());
            }
        }
    }
    for directory in ["usr/share/applications", "usr/share/thumbnailers"] {
        let directory = root.join(directory);
        if !directory.exists() {
            continue;
        }
        for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let text = std::fs::read_to_string(entry.path()).map_err(|error| error.to_string())?;
            for line in text.lines() {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let value = value.trim();
                if key.trim() == "DBusActivatable" && value.eq_ignore_ascii_case("true") {
                    return Err("App metadata cannot request unmanaged bus activation".into());
                }
                if matches!(key.trim(), "Exec" | "TryExec") {
                    let first = value
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_matches('"');
                    let program = first.strip_prefix("/usr/bin/").unwrap_or(first);
                    if !exports.contains_key(program) {
                        return Err("App launch metadata bypasses its declared Host export".into());
                    }
                }
            }
        }
    }
    Ok(())
}
