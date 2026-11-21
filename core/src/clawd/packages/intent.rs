//! Closed package-intent transport; identity and decisions are never inputs.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    Review,
    Submit,
    Status,
    Recover,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Action {
    Install,
    InstallRelease,
    FlatpakInstall,
    FlatpakUpdate,
    Remove,
    Purge,
    Upgrade,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::InstallRelease => "install-release",
            Self::FlatpakInstall => "flatpak-install",
            Self::FlatpakUpdate => "flatpak-update",
            Self::Remove => "remove",
            Self::Purge => "purge",
            Self::Upgrade => "upgrade",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub session: crate::clawd::wire::bounded::Token,
    pub invocation_id: crate::clawd::wire::bounded::Token<32>,
    pub mode: Mode,
    pub action: Action,
    pub packages: Vec<crate::clawd::wire::bounded::Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<crate::clawd::wire::bounded::Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flatpak: Option<super::flatpak::Target>,
}

impl Request {
    pub(super) fn names(&self) -> Result<Vec<String>, String> {
        let id = self.invocation_id.as_str();
        if id.len() != 32 || !id.bytes().all(|byte|
            byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        { return Err("invalid package intent correlation".into()); }
        let names = self.packages.iter().map(|name| name.as_str().to_string()).collect::<Vec<_>>();
        if matches!(self.action, Action::FlatpakInstall | Action::FlatpakUpdate) {
            if let Some(target) = &self.flatpak {
                if names.is_empty() && self.version.is_none() {
                    target.validate()?;
                    return Ok(names);
                }
            }
            return Err("A Flatpak intent requires one exact remote and application ref".into());
        }
        if self.flatpak.is_some() {
            return Err("Debian requests cannot contain Flatpak targets".into());
        }
        if names.is_empty() || names.len() > 128 {
            return Err("a package intent requires 1..128 exact Debian names".into());
        }
        for name in &names { super::validate_package_name(name)?; }
        let mut canonical = names.clone();
        canonical.sort();
        canonical.dedup();
        if canonical != names {
            return Err("package intent names must be sorted and unique".into());
        }
        match (self.action, self.version.as_ref(), names.as_slice()) {
            (Action::InstallRelease, Some(version), [name]) if crate::apps::store::app_package_name(name) => {
                super::validate_version(version.as_str())?;
            }
            (Action::InstallRelease, _, _) | (_, Some(_), _) => {
                return Err("An exact release requires one Claw App package and version".into());
            }
            _ => {}
        }
        Ok(names)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn operation(&self) -> Result<crate::clawd::package_transactions::Operation, String> {
        use crate::clawd::package_transactions::Operation;
        let packages = self.names()?;
        if let Some(target) = &self.flatpak {
            return Ok(Operation::Flatpak {
                install: self.action == Action::FlatpakInstall, target: target.clone(),
            });
        }
        if let (Action::InstallRelease, Some(version)) = (self.action, self.version.as_ref()) {
            return Ok(Operation::InstallRelease {
                package: packages[0].clone(), version: version.as_str().into(),
            });
        }
        if packages.len() == 1 {
            return Operation::from_action(self.action.as_str(), Some(&packages[0]), None);
        }
        Ok(match self.action {
            Action::Install => Operation::InstallPackages { packages },
            Action::InstallRelease => unreachable!("validated exact release"),
            Action::FlatpakInstall | Action::FlatpakUpdate => unreachable!("validated Flatpak target"),
            Action::Remove => Operation::RemovePackages { packages },
            Action::Purge => Operation::PurgePackages { packages },
            Action::Upgrade => Operation::UpgradePackages { packages },
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn capabilities(&self) -> Vec<crate::caps::Cap> {
        if let Some(target) = &self.flatpak {
            return vec![target.capability()];
        }
        self.packages.iter().map(|name| crate::caps::Cap::new(
            crate::caps::Verb::SYS_PACKAGE, crate::caps::Scope::name(name.as_str()))).collect()
    }
}

pub(crate) fn cli(args: &[String]) -> Result<Value, String> {
    if !(5..=132).contains(&args.len()) || args[0] != "--retained" {
        return Err("Invalid retained package intent arguments".into());
    }
    let session = std::env::var("COS_SESSION")
        .map_err(|_| "package intents require an authenticated GUI session")?;
    let (packages, version, flatpak) = if matches!(args[3].as_str(), "flatpak-install" | "flatpak-update") {
        let [scope, remote, reference] = &args[4..] else {
            return Err("Expected one Flatpak installation, remote and ref".into());
        };
        (Vec::new(), None, Some(json!({"scope": scope, "remote": remote, "ref": reference})))
    } else if args[3] == "install-release" {
        let (package, version) = args[4].split_once('=')
            .filter(|_| args.len() == 5).ok_or("Expected one exact package=version")?;
        (vec![package], Some(version), None)
    } else {
        (args[4..].iter().map(String::as_str).collect(), None, None)
    };
    let mut body = json!({
        "session": session, "mode": args[1], "invocation_id": args[2],
        "action": args[3], "packages": packages, "version": version,
    });
    if let Some(target) = flatpak { body["flatpak"] = target; }
    let request: Request = serde_json::from_value(body).map_err(|_| "invalid closed package intent")?;
    request.names()?;
    let response = crate::clawd::client::request_gui_resource_blocking(
        crate::clawd::protocol::Request::build(
            crate::clawd::routes::Command::GuiPackageIntent,
            serde_json::to_value(request).map_err(|error| error.to_string())?,
        ),
    ).map_err(|error| format!(
        "Root package intent response is unconfirmed: {error}. Query `cos package-transactions --invocation {}` outside Store; do not replay effects.", args[2]
    ))?;
    if response.ok {
        response.result.ok_or_else(|| "Root package intent response is missing".into())
    } else {
        Err(response.error.map(|error| error.message)
            .unwrap_or_else(|| "Root package intent was refused".into()))
    }
}
