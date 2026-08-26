//! Internal capability-policy bridge for bundled apps.
//!
//! This module is deliberately **not** exposed as a user-facing permission
//! CLI. Interactive permission decisions belong in the Agent UX; bundled apps
//! use this hidden bridge only to ask the kernel whether the current session is
//! allowed to perform one verb against one scope.

use serde_json::{json, Value};

use crate::caps::{require, Scope, Verb};

/// Entry point for the hidden policy-check bridge.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "check" => check(args),
        _ => Err(format!("unknown internal policy command: {command}")),
    }
}

/// Run one capability check for the current session.
///
/// The output envelope intentionally matches the legacy policy-check shape
/// consumed by `cos_runtime.policy`:
///
/// ```json
/// {"decision":"allow","verb":"fs.read","scope":{"kind":"path","value":"/tmp/x"}}
/// ```
fn check(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(
            "usage: internal policy check <verb> [--path <p> | --host <h> | --name <n> | --self <s> | --wild]"
                .into(),
        );
    }
    let verb_str = &args[0];
    let verb = Verb::parse(verb_str).ok_or_else(|| format!("unknown verb `{verb_str}`"))?;

    let mut scope: Option<Scope> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--path" if i + 1 < args.len() => {
                scope = Some(Scope::path(&args[i + 1]));
                i += 2;
            }
            "--host" if i + 1 < args.len() => {
                scope = Some(Scope::host(&args[i + 1]));
                i += 2;
            }
            "--name" if i + 1 < args.len() => {
                scope = Some(Scope::name(&args[i + 1]));
                i += 2;
            }
            "--self" if i + 1 < args.len() => {
                scope = Some(Scope::self_ref(&args[i + 1]));
                i += 2;
            }
            "--wild" => {
                scope = Some(Scope::Wild);
                i += 1;
            }
            other => return Err(format!("unexpected internal policy arg: {other}")),
        }
    }

    let scope = scope.unwrap_or(Scope::Wild);

    match require(verb, scope.clone()) {
        Ok(()) => Ok(json!({
            "decision": "allow",
            "verb": verb.as_str(),
            "scope": scope,
        })),
        Err(d) => {
            let mut obj = d.to_json();
            obj.as_object_mut()
                .map(|m| m.insert("decision".into(), Value::String("deny".into())));
            Ok(obj)
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/perms.rs"
    ));
}
