use serde::{Deserialize, Serialize};

use super::{Manifest, ManifestError, Need, NeedCondition, ScopeBinding, Verb};
use crate::i18n::LocalizedText;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopEntry {
    pub entry: String,
    pub exec: String,
    pub name: LocalizedText,
    pub needs: Vec<Need>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub optional_needs: Vec<Need>,
    #[serde(default)]
    pub panel_applet: bool,
}

/// A manifest-selected GUI surface, not execution or capability authority.
#[derive(Clone, Debug)]
pub struct GuiEntry<'a> {
    pub name: Option<&'a str>,
    pub entry: &'a str,
    pub exec: &'a str,
    pub label: &'a LocalizedText,
    pub needs: Vec<&'a Need>,
    pub optional_needs: Vec<&'a Need>,
    pub legacy_needs: bool,
    pub panel_applet: bool,
}

impl<'a> GuiEntry<'a> {
    pub fn cli_selector(&self) -> String {
        match self.name {
            Some(name) => format!("--gui:{name}"),
            None => self.exec.to_string(),
        }
    }

    /// GUI argv has no operation-argument binding. Explicit conditional
    /// needs remain unbound too; old manifests retain their original union.
    pub fn launch_needs(&self) -> impl Iterator<Item = &'a Need> + '_ {
        self.needs.iter().copied().filter(|need| self.need_is_bound(need))
    }

    pub fn need_is_bound(&self, need: &Need) -> bool {
        (self.legacy_needs || need.when.is_none())
            && matches!(need.scope, ScopeBinding::Fixed { .. } | ScopeBinding::Wild | ScopeBinding::OwnerPath { .. })
    }

    pub fn optional_need_is_bound(&self, need: &Need) -> bool {
        need.when.is_none()
            && matches!(need.scope, ScopeBinding::Fixed { .. } | ScopeBinding::Wild | ScopeBinding::OwnerPath { .. })
    }
}

pub fn is_valid_gui_entry_name(name: &str) -> bool {
    name.len() <= 64 && super::is_valid_id(name)
}

impl Manifest {
    /// Resolve only authenticated manifest data. A name is never a path,
    /// executable or selector supplied by the caller.
    pub fn gui_entry(&self, name: Option<&str>) -> Result<GuiEntry<'_>, String> {
        let desktop = self
            .desktop
            .as_ref()
            .ok_or_else(|| format!("App `{}` declares no desktop surface", self.id))?;
        if let Some(name) = name {
            if !is_valid_gui_entry_name(name) {
                return Err("GUI entry name must match [a-z][a-z0-9_-]* and fit 64 bytes".into());
            }
            let (name, entry) = desktop
                .entries
                .get_key_value(name)
                .ok_or_else(|| format!("App `{}` has no GUI entry `{name}`", self.id))?;
            return Ok(GuiEntry {
                name: Some(name.as_str()),
                entry: &entry.entry,
                exec: &entry.exec,
                label: &entry.name,
                needs: entry.needs.iter().collect(),
                optional_needs: entry.optional_needs.iter().collect(),
                legacy_needs: false,
                panel_applet: entry.panel_applet,
            });
        }
        Ok(GuiEntry {
            name: None,
            entry: desktop
                .entry
                .as_deref()
                .or(self.entry.as_deref())
                .unwrap_or_else(|| self.runtime.default_entry()),
            exec: &desktop.exec,
            label: desktop.name.as_ref().unwrap_or(&self.name),
            needs: match &desktop.needs {
                Some(needs) => needs.iter().collect(),
                None => self
                    .operations
                    .values()
                    .flat_map(|operation| &operation.needs)
                    .collect(),
            },
            legacy_needs: desktop.needs.is_none(),
            optional_needs: desktop.optional_needs.iter().collect(),
            panel_applet: desktop.panel_applet,
        })
    }

    /// Match the public GUI CLI namespace without interpreting user argv.
    pub fn gui_entry_for_cli(&self, selector: &str) -> Result<Option<GuiEntry<'_>>, String> {
        if self
            .desktop
            .as_ref()
            .is_some_and(|desktop| selector == desktop.exec || selector == "--gui")
        {
            return self.gui_entry(None).map(Some);
        }
        if let Some(name) = selector.strip_prefix("--gui:") {
            return self.gui_entry(Some(name)).map(Some);
        }
        Ok(None)
    }

    /// GUI targets to include in a package's signed entrypoint inventory,
    /// in addition to its independently selected operation and MCP entries.
    pub fn gui_entrypoints(&self) -> Result<Vec<&str>, String> {
        let Some(desktop) = &self.desktop else {
            return Ok(Vec::new());
        };
        let mut entries = vec![self.gui_entry(None)?.entry];
        for entry in desktop.entries.values() {
            if !entries.contains(&entry.entry.as_str()) {
                entries.push(&entry.entry);
            }
        }
        Ok(entries)
    }

    /// Permission inventory only, never a GUI launch capability union.
    pub fn declared_gui_needs(&self) -> impl Iterator<Item = &Need> {
        self.desktop.iter().flat_map(|desktop| {
            desktop.needs.iter().flatten()
                .chain(&desktop.optional_needs)
                .chain(desktop.entries.values().flat_map(|entry| entry.needs.iter().chain(&entry.optional_needs)))
        })
    }
}

pub(super) fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

pub(super) fn validate(manifest: &Manifest) -> Result<(), ManifestError> {
    let Some(desktop) = &manifest.desktop else {
        return Ok(());
    };
    if desktop.exec.len() > 1024 {
        return Err(invalid("exec", "must fit 1024 UTF-8 bytes"));
    }
    if let Some(entry) = &desktop.entry {
        validate_entry(entry).map_err(|detail| invalid("entry", &detail))?;
    }
    if let Some(needs) = &desktop.needs {
        validate_needs(manifest, "default", needs)?;
    }
    validate_needs(manifest, "default optional", &desktop.optional_needs)?;
    if desktop.entries.len() > 64 {
        return Err(invalid("entries", "at most 64 additional GUI entries are allowed"));
    }
    for (name, entry) in &desktop.entries {
        if !is_valid_gui_entry_name(name) {
            return Err(invalid(
                "entries",
                "names must match [a-z][a-z0-9_-]* and fit 64 bytes",
            ));
        }
        validate_entry(&entry.entry)
            .map_err(|detail| invalid("entries", &format!("{name}.entry: {detail}")))?;
        if entry.exec.trim().is_empty()
            || entry.exec.len() > 1024
            || entry.exec.chars().any(char::is_control)
        {
            return Err(invalid(
                "entries",
                &format!("{name}.exec must be nonempty, fit 1024 bytes and contain no controls"),
            ));
        }
        entry
            .name
            .validate()
            .map_err(|detail| invalid("entries", &format!("{name}.name: {detail}")))?;
        validate_needs(manifest, name, &entry.needs)?;
        validate_needs(manifest, &format!("{name} optional"), &entry.optional_needs)?;
    }
    Ok(())
}

fn validate_entry(entry: &str) -> Result<(), String> {
    let parts: Vec<_> = entry.split('/').collect();
    if entry.is_empty()
        || entry.len() > 512
        || !entry.is_ascii()
        || entry.contains('\\')
        || entry.chars().any(char::is_control)
        || parts.len() > 16
        || parts.iter().any(|part| {
            part.is_empty()
                || part.len() > 128
                || matches!(*part, "." | "..")
                || part.ends_with([' ', '.'])
        })
    {
        return Err("must be a canonical package-relative signed-tree path".into());
    }
    Ok(())
}

fn validate_needs(manifest: &Manifest, name: &str, needs: &[Need]) -> Result<(), ManifestError> {
    for (index, need) in needs.iter().enumerate() {
        let context = |detail| invalid("needs", &format!("{name} need #{index}: {detail}"));
        need.why.validate().map_err(context)?;
        super::validate_literal_path_scopes(&need.scope).map_err(context)?;
        if let ScopeBinding::OwnerPath { path } = &need.scope {
            super::owner_path::validate(need.verb, path).map_err(context)?;
        }
        if matches!(&need.when,
            Some(NeedCondition::ArgEquals { value, .. } | NeedCondition::ArgNotEquals { value, .. })
                if value.is_null()
        ) {
            return Err(context("condition value must not be null".into()));
        }
        if need.verb == Verb::AI_BYPASS {
            return Err(ManifestError::AiBypassNotAllowedForApps {
                field: "desktop GUI needs / optional_needs",
            });
        }
        if need.verb.as_str().starts_with("ai.") && manifest.ai.is_none() {
            return Err(context("AI needs require an App AI policy".into()));
        }
        if matches!(&need.scope, ScopeBinding::FromArgMap { values, .. } if values.is_empty()) {
            return Err(context("from-arg-map values must not be empty".into()));
        }
    }
    Ok(())
}

fn invalid(field: &'static str, detail: &str) -> ManifestError {
    ManifestError::DesktopInvalid {
        field,
        detail: detail.to_string(),
    }
}
