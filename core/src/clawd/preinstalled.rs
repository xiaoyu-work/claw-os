//! Default permissions for exact App builds selected by the OS packages.

use std::path::Path;

use crate::caps::{Cap, Risk, Scope, ScopeKind, Verb};
use crate::provenance::VerifiedPackage;

pub(super) fn allows(cap: &Cap, home: &Path) -> bool {
    let Some(meta) = crate::caps::lookup_meta(cap.verb) else {
        return false;
    };
    if meta.risk == Risk::Critical || matches!(
        cap.verb,
        Verb::AGENT_INVOKE | Verb::AGENT_SPAWN
            | Verb::SECRET_READ | Verb::SECRET_WRITE
            | Verb::SYS_PERMISSIONS | Verb::SYS_SERVICE
            | Verb::SYS_MOUNT | Verb::SYS_TIME | Verb::SYS_HOSTNAME
            | Verb::NET_RAW
    ) {
        return false;
    }
    if meta.scope_kind != ScopeKind::Path {
        return true;
    }
    let Scope::Path(path) = cap.scope.canonicalized() else {
        return false;
    };
    home.is_absolute() && home != Path::new("/") && Path::new(&path).starts_with(home)
}

#[cfg(target_os = "linux")]
pub(super) fn listed(package: &VerifiedPackage) -> bool {
    use crate::provenance::{fsec, PackageKind, TrustSource, TrustTier};

    if package.kind() != PackageKind::App
        || package.tier() != TrustTier::Vendor
        || !matches!(package.source(), TrustSource::Publisher { .. })
    {
        return false;
    }
    let Some(digest) = package.envelope_sha256() else {
        return false;
    };
    let Ok(relative) = package.dir().strip_prefix("/usr/lib/cos/apps") else {
        return false;
    };
    let Some(relative) = relative.to_str().filter(|path| !path.is_empty()) else {
        return false;
    };
    let root = Path::new("/usr/lib/cos/preinstalled-apps");
    if fsec::require_secure_location(root, &[0]).is_err() {
        return false;
    }
    let Ok(directory) = fsec::DirHandle::open(root) else {
        return false;
    };
    ["agent.list", "desktop.list"].iter().any(|name| {
        let Ok(file) = directory.open_file(name) else {
            return false;
        };
        let meta = file.meta();
        if meta.uid != 0 || meta.nlink != 1 || meta.is_group_or_world_writable() {
            return false;
        }
        let Ok(bytes) = file.read_bounded(64 * 1024) else {
            return false;
        };
        let Ok(list) = std::str::from_utf8(&bytes) else {
            return false;
        };
        list.lines().any(|line| line.split_once(' ') == Some((digest, relative)))
    })
}

#[cfg(target_os = "linux")]
pub(super) fn approved(package: &VerifiedPackage) -> bool {
    use crate::provenance::{envelope::ENVELOPE_FILE, fsec};

    listed(package)
        && fsec::require_secure_location(package.dir(), &[0]).is_ok()
        && package
            .files()
            .map(|file| file.path.as_str())
            .chain([ENVELOPE_FILE])
            .all(|path| fsec::lstat(&package.dir().join(path)).is_ok_and(|node| {
                node.uid == 0 && !node.is_symlink && !node.is_group_or_world_writable()
            }))
        && package.assert_current(&crate::provenance::trust_store()).is_ok()
}

#[cfg(not(target_os = "linux"))]
pub(super) fn approved(_package: &VerifiedPackage) -> bool {
    false
}
