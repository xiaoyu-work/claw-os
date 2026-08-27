//! Canonical binding of raw invocation arguments to a manifest's
//! declared [`Arg`] schema.
//!
//! Capability scopes for an App operation are *derived* from the values
//! the App will actually receive, so the derivation must happen exactly
//! once and identically everywhere. The unprivileged launcher uses it
//! to describe an invocation; `clawd` uses it to decide which caps the
//! App session is issued. If the two disagreed, the authority would
//! grant a scope the App never asked for — or the App would run with a
//! scope nobody authorised.
//!
//! The parser deliberately understands only the CLI shape the bridge
//! passes to Apps (`--name value`, `--name=value`, positionals in
//! declaration order) and produces a `name -> JSON value` map that
//! [`crate::caps::Manifest::resolve_needs`] turns into concrete
//! [`Scope`](crate::caps::Scope)s.

use std::collections::BTreeMap;

use serde_json::Value;

use super::manifest::{Arg, ArgBinding, ArgKind};

/// Bind raw CLI tokens to `decls`, the operation's declared arguments.
///
/// Only values the caller actually supplied are bound; manifest
/// defaults are applied separately by
/// [`Operation::apply_arg_defaults`](crate::caps::manifest::Operation)
/// so the launcher can also report them back as effective argv.
///
/// Unknown flags are rejected so only manifest-declared fields can reach an
/// App. Positional tokens bind only to positional declarations; remaining
/// tokens stay in argv for handlers with variadic positional behavior.
pub fn bind_supplied_cli_args(
    decls: &[Arg],
    raw: &[String],
) -> Result<BTreeMap<String, Value>, String> {
    let mut values = BTreeMap::new();
    let mut positionals = Vec::new();
    let mut index = 0;
    while index < raw.len() {
        let token = &raw[index];
        if let Some(flag) = token.strip_prefix("--") {
            let (raw_name, inline) = flag
                .split_once('=')
                .map(|(name, value)| (name, Some(value)))
                .unwrap_or((flag, None));
            let name = match_flag_name(decls, raw_name);
            if let Some(decl) = name.and_then(|name| decls.iter().find(|decl| decl.name == name)) {
                let value = inline.map(str::to_string).or_else(|| {
                    if decl.kind != ArgKind::Bool {
                        raw.get(index + 1)
                            .filter(|next| !next.starts_with("--"))
                            .cloned()
                    } else {
                        None
                    }
                });
                if inline.is_none() && value.is_some() {
                    index += 1;
                }
                let parsed = parse_arg_value(decl.kind, value.as_deref()).ok_or_else(|| {
                    format!(
                        "flag `--{}` requires a valid {} value",
                        flag_name(decl),
                        kind_label(decl.kind)
                    )
                })?;
                values.insert(decl.name.clone(), parsed);
            } else {
                return Err(format!("unknown operation flag `--{raw_name}`"));
            }
        } else {
            positionals.push(token.clone());
        }
        index += 1;
    }

    let mut positional = positionals.into_iter();
    for decl in decls {
        if values.contains_key(&decl.name)
            || decl.binding != ArgBinding::Positional
            || decl.kind == ArgKind::Bool
        {
            continue;
        }
        if let Some(raw) = positional.next() {
            if let Some(parsed) = parse_arg_value(decl.kind, Some(&raw)) {
                values.insert(decl.name.clone(), parsed);
            }
        }
    }
    Ok(values)
}

/// Bind raw CLI tokens and fill in the declared literal defaults.
///
/// The authority uses this over the *effective* argv a launcher reports
/// — defaults it resolved are already present as tokens there, so this
/// pass only backfills anything still unbound and gives booleans their
/// declared value.
pub fn bind_cli_args(
    decls: &[Arg],
    raw: &[String],
) -> Result<BTreeMap<String, Value>, String> {
    let mut values = bind_supplied_cli_args(decls, raw)?;
    for decl in decls {
        if values.contains_key(&decl.name) {
            continue;
        }
        if decl.kind == ArgKind::Bool {
            values.insert(
                decl.name.clone(),
                decl.default.clone().unwrap_or(Value::Bool(false)),
            );
        } else if let Some(default) = &decl.default {
            values.insert(decl.name.clone(), default.clone());
        }
    }
    Ok(values)
}

/// Where relative and `~`-prefixed path arguments resolve from.
///
/// Both the launcher and the authority resolve the same invocation, so
/// they must agree on this context. The launcher uses its own process
/// state; the authority uses the kernel's view of the peer — its passwd
/// home and `/proc/<pid>/cwd` — rather than anything the caller says.
#[derive(Debug, Clone)]
pub struct PathContext {
    pub home: std::path::PathBuf,
    pub cwd: Option<std::path::PathBuf>,
}

/// Rewrite every bound `path` argument to its absolute form.
///
/// A capability scope must name the resource the App will actually
/// touch, so `.`/`~/x`/`rel/x` are resolved before any scope is derived
/// from them.
pub fn resolve_path_args(
    decls: &[Arg],
    values: &mut BTreeMap<String, Value>,
    context: &PathContext,
) -> Result<(), String> {
    for decl in decls {
        if decl.kind != ArgKind::Path {
            continue;
        }
        let Some(value) = values.get(&decl.name).and_then(Value::as_str) else {
            continue;
        };
        let absolute = absolute_arg_path(value, context)?;
        values.insert(decl.name.clone(), Value::String(absolute));
    }
    Ok(())
}

pub fn absolute_arg_path(value: &str, context: &PathContext) -> Result<String, String> {
    let path = if value == "~" {
        context.home.clone()
    } else if let Some(rest) = value.strip_prefix("~/") {
        context.home.join(rest)
    } else {
        std::path::PathBuf::from(value)
    };
    let absolute = if path.is_absolute() {
        path
    } else {
        context
            .cwd
            .as_ref()
            .ok_or_else(|| "cannot resolve a relative path argument here".to_string())?
            .join(path)
    };
    let resolved = absolute.canonicalize().unwrap_or(absolute);
    resolved
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| "resolved path arg is not valid UTF-8".to_string())
}

/// Reject a bound argument map that does not satisfy the declared
/// schema. Run this before any authorization decision so a missing or
/// wrongly-typed value can never reach scope derivation.
///
/// Undeclared entries are ignored rather than rejected: scope
/// derivation only ever reads names the manifest declares, so an extra
/// key cannot influence the issued capabilities, and MCP callers
/// legitimately carry protocol metadata alongside the declared
/// arguments.
pub fn validate_bound_args(decls: &[Arg], values: &BTreeMap<String, Value>) -> Result<(), String> {
    for decl in decls {
        match values.get(&decl.name) {
            Some(value) if !value_matches_kind(decl.kind, value) => {
                return Err(format!(
                    "argument `{}` is not a valid {}",
                    decl.name,
                    kind_label(decl.kind)
                ));
            }
            None if decl.required => {
                return Err(format!("argument `{}` is required", decl.name));
            }
            _ => {}
        }
    }
    Ok(())
}

fn value_matches_kind(kind: ArgKind, value: &Value) -> bool {
    match kind {
        ArgKind::Path | ArgKind::Host | ArgKind::Name | ArgKind::Text => value.is_string(),
        ArgKind::Number => value.is_number(),
        ArgKind::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        ArgKind::Bool => value.is_boolean(),
    }
}

fn kind_label(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::Path => "path",
        ArgKind::Host => "host",
        ArgKind::Name => "name",
        ArgKind::Text => "text",
        ArgKind::Number => "number",
        ArgKind::Integer => "integer",
        ArgKind::Bool => "boolean",
    }
}

fn match_flag_name<'a>(decls: &'a [Arg], raw: &str) -> Option<&'a str> {
    decls
        .iter()
        .filter(|decl| decl.binding == ArgBinding::Flag)
        .find(|decl| decl.name == raw || flag_name(decl) == raw)
        .map(|decl| decl.name.as_str())
}

pub fn flag_name(arg: &Arg) -> String {
    arg.name.replace('_', "-")
}

fn parse_arg_value(kind: ArgKind, raw: Option<&str>) -> Option<Value> {
    match kind {
        ArgKind::Bool => Some(Value::Bool(
            raw.map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(true),
        )),
        ArgKind::Number => raw
            .and_then(|value| value.parse::<f64>().ok())
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
        ArgKind::Integer => raw
            .and_then(|value| value.parse::<i64>().ok())
            .map(serde_json::Number::from)
            .map(Value::Number),
        ArgKind::Path | ArgKind::Host | ArgKind::Name | ArgKind::Text => {
            raw.map(|value| Value::String(value.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/caps/args.rs"));
}
