//! Read-only package impact evidence. Unmodelled Root code never becomes a precise closure.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Cursor;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::clawd::app_store::{self, apt};
use crate::clawd::state::DaemonState;
use crate::provenance::runtime::PackageRef;

use super::backend::Plan;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Impact {
    pub plan_digest: String,
    pub digest: String,
    pub facts_digest: String,
    pub static_digest: String,
    pub affected_apps: Vec<String>,
    pub all_apps: Vec<String>,
    pub uncertainties: Vec<String>,
    pub evidence: Vec<String>,
}

impl Impact {
    pub(super) fn global(&self) -> bool {
        !self.uncertainties.is_empty()
    }

    pub(super) fn needs_confirmation(&self) -> bool {
        self.global() && !self.all_apps.is_empty()
    }

    pub(super) fn retirement_apps(&self) -> &[String] {
        if self.global() {
            &self.all_apps
        } else {
            &self.affected_apps
        }
    }

    pub(super) fn summary(&self) -> Value {
        json!({
            "impact_digest": self.digest,
            "plan_digest": self.plan_digest,
            "scope": if self.global() { "global_confirmation_required" } else { "affected_apps" },
            "known_affected_apps": self.affected_apps,
            "all_apps": self.all_apps,
            "uncertainties": self.uncertainties,
            "evidence": self.evidence,
        })
    }
}

#[derive(Clone)]
struct Package {
    id: String,
    name: String,
    arch: String,
    foreign: bool,
    dependencies: Vec<(Vec<String>, bool)>,
    provides: Vec<String>,
}

fn relations(value: &str) -> Vec<(Vec<String>, bool)> {
    value
        .split(',')
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let names = part
                .split('|')
                .filter_map(|alternative| {
                    alternative
                        .trim()
                        .split_whitespace()
                        .next()
                        .map(str::to_string)
                })
                .collect::<Vec<_>>();
            let ambiguous = names.len() != 1 || part.contains('[') || part.contains('<');
            (names, ambiguous)
        })
        .collect()
}

fn package(row: &str) -> Result<Package, String> {
    let columns = row.split('\t').collect::<Vec<_>>();
    if columns.len() != 8 {
        return Err("Installed package dependency metadata is incomplete".into());
    }
    crate::clawd::packages::validate_package_name(columns[0])?;
    crate::clawd::packages::validate_package_name(&format!("fixture:{}", columns[2]))?;
    let name = columns[0].split(':').next().unwrap().to_string();
    let mut dependencies = relations(columns[4]);
    dependencies.extend(relations(columns[5]));
    let provides = relations(columns[6])
        .into_iter()
        .flat_map(|(names, _)| names)
        .map(|name| name.split(':').next().unwrap_or(&name).to_string())
        .collect();
    Ok(Package {
        id: format!("{}:{}", name, columns[2]),
        name,
        arch: columns[2].into(),
        foreign: columns[7] == "foreign",
        dependencies,
        provides,
    })
}

fn dependent_packages(
    nodes: &[Package],
    changed: &BTreeSet<String>,
    native: &str,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut reverse: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
    for node in nodes {
        for (alternatives, uncertain) in &node.dependencies {
            let mut candidates = BTreeSet::new();
            for name in alternatives {
                let (base, selected) = name
                    .split_once(':')
                    .map_or((name.as_str(), None), |(name, arch)| (name, Some(arch)));
                let preferred = if node.arch == "all" {
                    native
                } else {
                    node.arch.as_str()
                };
                for provider in nodes.iter().filter(|provider| {
                    provider.name == base || provider.provides.iter().any(|name| name == base)
                }) {
                    let allowed = match selected {
                        Some("any") => true,
                        Some("native") => provider.arch == native || provider.arch == "all",
                        Some(arch) => provider.arch == arch || provider.arch == "all",
                        None => {
                            provider.arch == preferred || provider.arch == "all" || provider.foreign
                        }
                    };
                    if allowed {
                        candidates.insert(provider.id.clone());
                    }
                }
            }
            let uncertain = *uncertain || candidates.len() > 1;
            for target in candidates {
                reverse
                    .entry(target)
                    .or_default()
                    .push((node.id.clone(), uncertain));
            }
        }
    }
    let mut closure = changed.clone();
    let mut queue: VecDeque<_> = changed.iter().cloned().collect();
    let mut uncertainties = BTreeSet::new();
    while let Some(target) = queue.pop_front() {
        for (dependent, uncertain) in reverse.get(&target).into_iter().flatten() {
            if *uncertain {
                uncertainties.insert(format!("The installed dependency alternative/provider for {dependent} is not a unique runtime-use proof"));
            }
            if closure.insert(dependent.clone()) {
                queue.push_back(dependent.clone());
            }
        }
    }
    (closure, uncertainties)
}

fn absolute(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value.trim());
    let mut result = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(value) => result.push(value),
            _ => return Err("Package file inventory contains an unbounded path".into()),
        }
    }
    if value.chars().any(char::is_control) || result.as_os_str().len() > 4096 {
        return Err("Package file inventory contains an invalid path".into());
    }
    Ok(result)
}

fn tail_after_words(mut line: &str, count: usize) -> Result<&str, String> {
    for _ in 0..count {
        line = line.trim_start();
        let end = line
            .find(char::is_whitespace)
            .ok_or("Incomplete Debian file inventory")?;
        line = &line[end..];
    }
    Ok(line.trim_start())
}

fn archive_paths(text: &str) -> Result<BTreeSet<PathBuf>, String> {
    let mut paths = BTreeSet::new();
    for line in text.lines() {
        let mode = line
            .split_whitespace()
            .next()
            .ok_or("Missing Debian file type")?;
        let tail = tail_after_words(line, 5)?;
        if !mode.starts_with('h') && !mode.starts_with('l') && tail.contains(" link to ") {
            return Err(
                "The text inventory does not distinguish a hardlink from its pathname".into(),
            );
        }
        let (name, linked) = if mode.starts_with('l') {
            (
                tail.split_once(" -> ")
                    .ok_or("Invalid Debian symlink inventory")?
                    .0,
                None,
            )
        } else if mode.starts_with('h') {
            let (name, target) = tail
                .split_once(" link to ")
                .ok_or("Invalid Debian hardlink inventory")?;
            (name, Some(target))
        } else {
            (tail, None)
        };
        let path = absolute(name)?;
        if mode.starts_with('d') {
            let stable_parent = matches!(
                path.to_str(),
                Some("/" | "/usr" | "/usr/lib" | "/usr/lib/cos" | "/usr/lib/cos/apps")
            );
            if stable_parent
                && mode == "drwxr-xr-x"
                && matches!(line.split_whitespace().nth(1), Some("root/root" | "0/0"))
            {
                let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
                if metadata.uid() == 0 && metadata.gid() == 0 && metadata.mode() & 0o7777 == 0o755 {
                    continue;
                }
            }
        }
        paths.insert(path);
        if let Some(target) = linked {
            paths.insert(absolute(target)?);
        }
        if paths.len() > 20000 {
            return Err("Package file inventory exceeds its bounded capacity".into());
        }
    }
    Ok(paths)
}

fn root_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 1024 * 1024
    {
        return Err("Installed package impact evidence is not protected and bounded".into());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(Some(bytes))
}

async fn facts(
    plan: &Plan,
    id: &str,
) -> Result<
    (
        Vec<Package>,
        BTreeSet<PathBuf>,
        Vec<String>,
        BTreeSet<String>,
    ),
    String,
> {
    let mut evidence = Vec::new();
    let mut uncertain = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut nodes = Vec::new();
    let installed = apt::run("/usr/bin/dpkg-query", vec![
        "--show".into(),
        "--showformat=${binary:Package}\\t${Version}\\t${Architecture}\\t${db:Status-Status}\\t${Depends}\\t${Pre-Depends}\\t${Provides}\\t${Multi-Arch}\\n".into(),
    ], None, None).await?;
    evidence.push(crate::crypto::sha256_hex(&installed.stdout));
    for row in installed.stdout_string().lines() {
        if nodes.len() >= 8192 {
            return Err("Installed dependency metadata exceeds its bound".into());
        }
        let status = row.split('\t').nth(3).unwrap_or_default();
        if matches!(status, "installed" | "config-files") {
            nodes.push(package(row)?);
        }
    }
    for action in &plan.actions {
        if action.old_version.is_none() && action.version.is_some() {
            continue;
        }
        let listed = apt::run(
            "/usr/bin/dpkg-query",
            vec!["--listfiles".into(), "--".into(), action.query.clone()],
            None,
            None,
        )
        .await?;
        evidence.push(crate::crypto::sha256_hex(&listed.stdout));
        for line in listed.stdout_string().lines() {
            let path = absolute(line)?;
            if path.is_dir()
                && matches!(
                    path.to_str(),
                    Some("/" | "/usr" | "/usr/lib" | "/usr/lib/cos" | "/usr/lib/cos/apps")
                )
            {
                continue;
            }
            paths.insert(path);
            if paths.len() > 20000 {
                return Err("Complete package path impact exceeds its bound".into());
            }
        }
        let controls = apt::run(
            "/usr/bin/dpkg-query",
            vec!["--control-path".into(), action.query.clone()],
            None,
            None,
        )
        .await?;
        evidence.push(crate::crypto::sha256_hex(&controls.stdout));
        for path in controls.stdout_string().lines() {
            let path = absolute(path)?;
            if !path.starts_with("/var/lib/dpkg/info") {
                return Err(
                    "Installed control metadata is outside the protected dpkg store".into(),
                );
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if [
                "preinst", "postinst", "prerm", "postrm", "config", "triggers",
            ]
            .iter()
            .any(|suffix| name.ends_with(&format!(".{suffix}")))
            {
                let bytes = root_file(&path)?.ok_or("Installed helper evidence disappeared")?;
                evidence.push(crate::crypto::sha256_hex(&bytes));
                uncertain.insert(format!(
                    "Installed maintainer/trigger code for {} has unbounded effects",
                    action.package
                ));
            }
        }
    }
    for prepared in &plan.delivery.packages {
        let file = super::backend::cache(id)?.join(&prepared.file);
        let listing = apt::run(
            "/usr/bin/dpkg-deb",
            vec!["--contents".into(), file.to_string_lossy().into()],
            None,
            None,
        )
        .await?;
        evidence.push(crate::crypto::sha256_hex(&listing.stdout));
        paths.extend(archive_paths(&listing.stdout_string())?);
        if paths.len() > 20000 {
            return Err("Complete package path impact exceeds its bound".into());
        }
        let controls = apt::run(
            "/usr/bin/dpkg-deb",
            vec!["--ctrl-tarfile".into(), file.to_string_lossy().into()],
            None,
            None,
        )
        .await?;
        evidence.push(crate::crypto::sha256_hex(&controls.stdout));
        let mut archive = tar::Archive::new(Cursor::new(&controls.stdout));
        for entry in archive.entries().map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.path().map_err(|error| error.to_string())?;
            if matches!(
                name.file_name().and_then(|name| name.to_str()),
                Some("preinst" | "postinst" | "prerm" | "postrm" | "config" | "triggers")
            ) {
                uncertain.insert(format!(
                    "Prepared maintainer/trigger code for {} has unbounded effects",
                    prepared.identity.package
                ));
            }
        }
        let metadata = apt::run("/usr/bin/dpkg-deb", vec![
            "--show".into(),
            "--showformat=${Package}\\t${Version}\\t${Architecture}\\tinstalled\\t${Depends}\\t${Pre-Depends}\\t${Provides}\\t${Multi-Arch}\\n".into(),
            file.to_string_lossy().into(),
        ], None, None).await?;
        evidence.push(crate::crypto::sha256_hex(&metadata.stdout));
        for row in metadata.stdout_string().lines() {
            nodes.push(package(row)?);
        }
    }
    let config = apt::run("/usr/bin/apt-config", vec!["dump".into()], None, None).await?;
    evidence.push(crate::crypto::sha256_hex(&config.stdout));
    if config.stdout_string().lines().any(|line| {
        [
            "DPkg::Pre-Invoke::",
            "DPkg::Post-Invoke::",
            "DPkg::Pre-Install-Pkgs::",
        ]
        .iter()
        .any(|key| line.contains(key))
    }) {
        uncertain.insert(
            "Configured Root APT helpers have effects outside a package dependency closure".into(),
        );
    }
    if config.stdout_string().lines().any(|line| {
        let line = line.trim();
        line.contains("Dir::Bin::dpkg ")
            && !line.ends_with("\"/usr/bin/dpkg\";")
            && !line.ends_with("\"dpkg\";")
    }) {
        uncertain.insert(
            "The configured dpkg helper is outside the bounded installer implementation".into(),
        );
    }
    let triggers = root_file(Path::new("/var/lib/dpkg/triggers/File"))?.unwrap_or_default();
    evidence.push(crate::crypto::sha256_hex(&triggers));
    let triggers =
        std::str::from_utf8(&triggers).map_err(|_| "Invalid installed trigger metadata")?;
    for line in triggers.lines() {
        let prefix = line
            .split_whitespace()
            .next()
            .ok_or("Invalid installed trigger entry")?;
        if prefix.starts_with('/') && paths.iter().any(|path| path.starts_with(prefix)) {
            uncertain.insert(
                "Installed file triggers can execute Root helpers for the changed payload paths"
                    .into(),
            );
        }
    }
    if let Some(pending) = root_file(Path::new("/var/lib/dpkg/triggers/Unincorp"))? {
        evidence.push(crate::crypto::sha256_hex(&pending));
        if pending.iter().any(|byte| !byte.is_ascii_whitespace()) {
            uncertain.insert(
                "Pending dpkg trigger effects are not a bounded App dependency closure".into(),
            );
        }
    }
    Ok((nodes, paths, evidence, uncertain))
}

pub(super) async fn inspect(state: &DaemonState, plan: &Plan, id: &str) -> Result<Impact, String> {
    if !plan.payload_effects() {
        return seal(
            plan,
            Vec::new(),
            Vec::new(),
            BTreeSet::new(),
            Vec::new(),
            json!({}),
        );
    }
    let bindings = super::super::trust_withdrawal::retained_bindings(state, None).await?;
    let discovered = crate::apps::discover_all(&app_store::apps_root());
    let apps = discovered.all();
    let all_apps: BTreeSet<_> = apps
        .keys()
        .cloned()
        .chain(bindings.iter().map(|binding| binding.app.clone()))
        .collect();
    let mut identities = BTreeMap::new();
    let mut roots = BTreeMap::new();
    let mut owners = BTreeMap::new();
    for (app_id, app) in &apps {
        identities.insert(
            app_id.clone(),
            app.require_verified()
                .ok()
                .map(|package| PackageRef::of(package)),
        );
        roots.insert(app_id.clone(), app.dir.clone());
        owners.insert(
            app_id.clone(),
            crate::clawd::packages::query_package_owners(&app.dir.join("app.json")).await?,
        );
    }
    for review in &plan.delivery.reviews {
        let marker = Path::new("usr/lib/cos/apps");
        let components = review.source.components().collect::<Vec<_>>();
        let relative = components
            .windows(4)
            .position(|part| {
                part.iter()
                    .map(|part| part.as_os_str())
                    .eq(marker.components().map(|part| part.as_os_str()))
            })
            .map(|index| components[index + 4..].iter().collect::<PathBuf>())
            .ok_or("Prepared App has no authenticated installed payload path")?;
        roots.insert(review.app_id.clone(), app_store::apps_root().join(relative));
    }
    let (nodes, paths, evidence, mut uncertain) = match facts(plan, id).await {
        Ok(facts) => facts,
        Err(_) => {
            let mut reasons = BTreeSet::new();
            reasons.insert(
                "The complete package/helper/dependency impact evidence could not be bounded"
                    .into(),
            );
            (Vec::new(), BTreeSet::new(), Vec::new(), reasons)
        }
    };
    let changed = plan
        .actions
        .iter()
        .flat_map(|action| {
            [
                action.old_architecture.as_ref(),
                action.architecture.as_ref(),
            ]
            .into_iter()
            .flatten()
            .map(|arch| format!("{}:{arch}", action.package))
            .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    let native = crate::clawd::packages::native_architecture().await?;
    let (dependent, ambiguous) = dependent_packages(&nodes, &changed, &native);
    uncertain.extend(ambiguous);
    let mut affected: BTreeSet<_> = plan
        .delivery
        .reviews
        .iter()
        .map(|review| review.app_id.clone())
        .collect();
    affected.extend(plan.removed_apps.iter().map(|app| app.id.clone()));
    let mut descriptions = Vec::new();
    for (app, packages) in &owners {
        for owner in packages {
            let (base, selected) = owner
                .split_once(':')
                .map_or((owner.as_str(), None), |(name, arch)| (name, Some(arch)));
            if nodes.iter().any(|node| {
                node.name == base
                    && dependent.contains(&node.id)
                    && selected.map_or(node.arch == native || node.arch == "all", |arch| {
                        node.arch == arch || node.arch == "all"
                    })
            }) {
                affected.insert(app.clone());
                descriptions.push(format!("{app}: installed package dependency on {base} is in the concrete change closure"));
            }
        }
    }
    for path in &paths {
        let matched = roots
            .iter()
            .filter(|(_, root)| path.starts_with(root))
            .max_by_key(|(_, root)| root.components().count());
        if let Some((app, _)) = matched {
            affected.insert(app.clone());
        } else {
            uncertain.insert("Changed resources are outside a private App payload; declared package dependencies do not prove exclusive runtime users".into());
        }
    }
    // worker::linux's common read-only image exposes /usr. A declared Debian
    // dependency is not an exclusive mounted-file user inventory.
    if paths.iter().any(|path| path.starts_with("/usr"))
        && all_apps.iter().any(|app| !affected.contains(app))
    {
        uncertain.insert("The common read-only /usr runtime exposes changed files beyond the known package dependency users; exclusive App readers are not recorded".into());
    }
    for binding in &bindings {
        if binding.package.is_none()
            || identities
                .get(&binding.app)
                .is_none_or(|current| current.as_ref() != binding.package.as_ref())
        {
            uncertain.insert(
                "Retained App custody has an older or unknown artifact/dependency snapshot".into(),
            );
        }
    }
    descriptions.extend(uncertain.iter().cloned());
    let retained = bindings
        .iter()
        .filter(|binding| {
            binding.package.is_none()
                || identities
                    .get(&binding.app)
                    .is_none_or(|current| current.as_ref() != binding.package.as_ref())
        })
        .map(|binding| {
            serde_json::to_string(&(&binding.app, &binding.package))
                .map_err(|error| error.to_string())
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let snapshot = json!({
        "identities": identities,
        "roots": roots,
        "owners": owners,
        "evidence": evidence,
        "paths": paths,
    });
    let mut result = seal(
        plan,
        affected.into_iter().collect(),
        all_apps.into_iter().collect(),
        uncertain,
        descriptions,
        snapshot,
    )?;
    result.facts_digest = crate::crypto::sha256_hex(
        &serde_json::to_vec(&(&result.static_digest, retained))
            .map_err(|error| error.to_string())?,
    );
    result.digest = binding_digest(&result)?;
    Ok(result)
}

fn seal(
    plan: &Plan,
    affected_apps: Vec<String>,
    all_apps: Vec<String>,
    uncertainties: BTreeSet<String>,
    evidence: Vec<String>,
    facts: Value,
) -> Result<Impact, String> {
    let facts_digest =
        crate::crypto::sha256_hex(&serde_json::to_vec(&facts).map_err(|error| error.to_string())?);
    let uncertainties = uncertainties.into_iter().collect::<Vec<_>>();
    let mut result = Impact {
        plan_digest: plan.delivery.digest.clone(),
        digest: String::new(),
        static_digest: facts_digest.clone(),
        facts_digest,
        affected_apps,
        all_apps,
        uncertainties,
        evidence,
    };
    result.digest = binding_digest(&result)?;
    Ok(result)
}

fn binding_digest(impact: &Impact) -> Result<String, String> {
    Ok(crate::crypto::sha256_hex(
        &serde_json::to_vec(&(
            &impact.plan_digest,
            &impact.facts_digest,
            &impact.affected_apps,
            &impact.all_apps,
            &impact.uncertainties,
        ))
        .map_err(|error| error.to_string())?,
    ))
}

pub(super) async fn verify_static(impact: &Impact, plan: &Plan, id: &str) -> Result<(), String> {
    if impact.global() {
        return Ok(());
    }
    let (nodes, paths, evidence, uncertain) = facts(plan, id).await?;
    let _ = nodes;
    if !uncertain.is_empty() {
        return Err("The bounded impact gained unconfirmed helper or trigger effects".into());
    }
    let apps = crate::apps::discover_all(&app_store::apps_root()).all();
    let mut identities = BTreeMap::new();
    let mut roots = BTreeMap::new();
    let mut owners = BTreeMap::new();
    for (app_id, app) in apps {
        identities.insert(
            app_id.clone(),
            app.require_verified()
                .ok()
                .map(|package| PackageRef::of(package)),
        );
        owners.insert(
            app_id.clone(),
            crate::clawd::packages::query_package_owners(&app.dir.join("app.json")).await?,
        );
        roots.insert(app_id, app.dir);
    }
    for review in &plan.delivery.reviews {
        let parts = review.source.components().collect::<Vec<_>>();
        let marker = Path::new("usr/lib/cos/apps");
        let index = parts
            .windows(4)
            .position(|part| {
                part.iter()
                    .map(|part| part.as_os_str())
                    .eq(marker.components().map(|part| part.as_os_str()))
            })
            .ok_or("Prepared App has no authenticated installed payload path")?;
        roots.insert(
            review.app_id.clone(),
            app_store::apps_root().join(parts[index + 4..].iter().collect::<PathBuf>()),
        );
    }
    let snapshot = json!({"identities":identities,"roots":roots,"owners":owners,"evidence":evidence,"paths":paths});
    let digest = crate::crypto::sha256_hex(
        &serde_json::to_vec(&snapshot).map_err(|error| error.to_string())?,
    );
    if digest != impact.static_digest {
        return Err("Package/App/dependency impact evidence changed before dpkg".into());
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn dependency_fixture(
    rows: &[&str],
    changed: &[&str],
) -> Result<(Vec<String>, bool), String> {
    let nodes = rows
        .iter()
        .map(|row| package(row))
        .collect::<Result<Vec<_>, _>>()?;
    let changed = changed.iter().map(|name| name.to_string()).collect();
    let (closure, uncertain) = dependent_packages(&nodes, &changed, "amd64");
    Ok((closure.into_iter().collect(), !uncertain.is_empty()))
}
