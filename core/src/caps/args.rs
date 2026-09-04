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
/// Unknown flags and excess positional tokens are rejected so only
/// manifest-declared fields can reach an App. Variadic behavior must be
/// declared with `repeatable`.
pub fn bind_supplied_cli_args(
    decls: &[Arg],
    raw: &[String],
) -> Result<BTreeMap<String, Value>, String> {
    let mut values = BTreeMap::new();
    let mut positionals = Vec::new();
    let mut index = 0;
    let mut options = true;
    while index < raw.len() {
        let token = &raw[index];
        if options && token == "--" {
            options = false;
        } else if options {
            let Some((decl, inline)) = match_option_decl(decls, token) else {
                if token.starts_with('-') && token != "-" {
                    return Err(format!("unknown operation flag `{token}`"));
                }
                positionals.push(token.clone());
                index += 1;
                continue;
            };
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
                    "option `{token}` requires a valid {} value",
                    kind_label(decl.kind)
                )
            })?;
            insert_supplied_value(&mut values, decl, parsed)?;
        } else {
            positionals.push(token.clone());
        }
        index += 1;
    }

    let positional_decls = decls
        .iter()
        .filter(|decl| decl.effective_binding() == ArgBinding::Positional)
        .collect::<Vec<_>>();
    let mut positional = positionals.into_iter();
    for decl in positional_decls {
        if values.contains_key(&decl.name) || decl.effective_binding() != ArgBinding::Positional {
            continue;
        }
        if decl.repeatable {
            let parsed = positional
                .by_ref()
                .map(|raw| parse_declared_value(decl, Some(&raw)))
                .collect::<Result<Vec<_>, _>>()?;
            if !parsed.is_empty() {
                values.insert(decl.name.clone(), Value::Array(parsed));
            }
        } else if let Some(raw) = positional.next() {
            let parsed = parse_declared_value(decl, Some(&raw))?;
            values.insert(decl.name.clone(), parsed);
        }
    }
    if let Some(extra) = positional.next() {
        return Err(format!(
            "unexpected positional operation argument `{extra}`"
        ));
    }
    Ok(values)
}

/// Bind raw CLI tokens and fill in the declared literal defaults.
///
/// The authority uses this over the *effective* argv a launcher reports
/// — defaults it resolved are already present as tokens there, so this
/// pass only backfills anything still unbound and gives booleans their
/// declared value.
pub fn bind_cli_args(decls: &[Arg], raw: &[String]) -> Result<BTreeMap<String, Value>, String> {
    let mut values = bind_supplied_cli_args(decls, raw)?;
    for decl in decls {
        if values.contains_key(&decl.name) {
            continue;
        }
        if let Some(default) = &decl.default {
            values.insert(decl.name.clone(), default.clone());
        }
    }
    if let Some(disallowed) = decls.iter().find(|decl| {
        decl.required_when.as_ref().is_some_and(|condition| {
            !super::manifest::condition_applies(Some(condition), &values)
                && values.contains_key(&decl.name)
        })
    }) {
        return Err(format!(
            "argument `{}` is only accepted when required_when applies",
            disallowed.name
        ));
    }
    if let Some(required) = decls.iter().find(|decl| {
        super::manifest::argument_is_required(decl, &values) && !values.contains_key(&decl.name)
    }) {
        return Err(format!("argument `{}` is required", required.name));
    }
    validate_bound_args(decls, &values)?;
    for decl in decls {
        if decl.kind == ArgKind::Bool
            && decl.required_when.is_none()
            && !values.contains_key(&decl.name)
        {
            values.insert(decl.name.clone(), Value::Bool(false));
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
        let Some(value) = values.get(&decl.name) else {
            continue;
        };
        if decl.repeatable {
            let resolved = value
                .as_array()
                .ok_or_else(|| format!("argument `{}` must be an array", decl.name))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| format!("argument `{}` must contain paths", decl.name))
                        .and_then(|value| absolute_arg_path(value, context))
                        .map(Value::String)
                })
                .collect::<Result<Vec<_>, _>>()?;
            values.insert(decl.name.clone(), Value::Array(resolved));
        } else if let Some(value) = value.as_str() {
            let absolute = absolute_arg_path(value, context)?;
            values.insert(decl.name.clone(), Value::String(absolute));
        }
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
pub fn validate_bound_args(decls: &[Arg], values: &BTreeMap<String, Value>) -> Result<(), String> {
    if let Some(name) = values
        .keys()
        .find(|name| !decls.iter().any(|decl| decl.name == name.as_str()))
    {
        return Err(format!("unknown argument `{name}`"));
    }
    for decl in decls {
        match values.get(&decl.name) {
            Some(Value::Array(values)) if decl.required && values.is_empty() => {
                return Err(format!("argument `{}` is required", decl.name));
            }
            Some(value) if !value_matches_declaration(decl, value) => {
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

fn value_matches_declaration(decl: &Arg, value: &Value) -> bool {
    let scalar_matches = |value: &Value| {
        value_matches_kind(decl.kind, value)
            && (decl.choices.is_empty() || decl.choices.contains(value))
    };
    if decl.repeatable {
        value
            .as_array()
            .is_some_and(|values| values.iter().all(scalar_matches))
    } else {
        scalar_matches(value)
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

fn match_option_decl<'a>(decls: &'a [Arg], token: &'a str) -> Option<(&'a Arg, Option<&'a str>)> {
    let (option, inline) = token
        .split_once('=')
        .map(|(option, value)| (option, Some(value)))
        .unwrap_or((token, None));
    decls
        .iter()
        .find(|decl| {
            decl.effective_binding() == ArgBinding::Flag
                && option == format!("--{}", flag_name(decl))
        })
        .map(|decl| (decl, inline))
}

pub fn flag_name(arg: &Arg) -> String {
    arg.name.replace('_', "-")
}

fn parse_arg_value(kind: ArgKind, raw: Option<&str>) -> Option<Value> {
    match kind {
        ArgKind::Bool => Some(Value::Bool(
            match raw.map(|value| value.trim().to_ascii_lowercase()) {
                None => true,
                Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on") => true,
                Some(value) if matches!(value.as_str(), "0" | "false" | "no" | "off") => false,
                Some(_) => return None,
            },
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

fn parse_declared_value(decl: &Arg, raw: Option<&str>) -> Result<Value, String> {
    let parsed = parse_arg_value(decl.kind, raw).ok_or_else(|| {
        format!(
            "argument `{}` is not a valid {}",
            decl.name,
            kind_label(decl.kind)
        )
    })?;
    if !decl.choices.is_empty() && !decl.choices.contains(&parsed) {
        return Err(format!(
            "argument `{}` must be one of {}",
            decl.name,
            decl.choices
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(parsed)
}

fn insert_supplied_value(
    values: &mut BTreeMap<String, Value>,
    decl: &Arg,
    parsed: Value,
) -> Result<(), String> {
    if !decl.choices.is_empty() && !decl.choices.contains(&parsed) {
        return Err(format!(
            "argument `{}` must be one of {}",
            decl.name,
            decl.choices
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if decl.repeatable {
        values
            .entry(decl.name.clone())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("repeatable values are initialized as arrays")
            .push(parsed);
        Ok(())
    } else if values.insert(decl.name.clone(), parsed).is_some() {
        Err(format!(
            "argument `{}` was supplied more than once but is not repeatable",
            decl.name
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/args.rs"
    ));
}
