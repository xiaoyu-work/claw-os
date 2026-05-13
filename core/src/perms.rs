//! The `cos perms ...` subcommand suite.
//!
//! This is the user- and app-facing CLI surface for the capability
//! system. Today it exposes one verb (`check`); the design space for
//! the rest is in the master plan doc (perms list / show / revoke /
//! audit / undo).
//!
//! The Python helper at `apps/_lib/policy.py` shells out to
//! `cos perms check <verb> --<scope-kind> <value>` to gate operations
//! inside Python apps. The JSON envelope here is therefore part of a
//! stable contract — keep the shape backwards compatible.

use serde_json::{json, Value};

use crate::caps::{require, Scope, Verb};

/// Entry point for `cos perms <subcommand> [args…]`. Wired in
/// `router.rs`.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "check" => cmd_check(args),
        _ => Err(format!("unknown perms command: {command}")),
    }
}

// ---------------------------------------------------------------------------
// perms check
// ---------------------------------------------------------------------------

/// Run a single capability check from the command line.
///
/// ```text
/// cos perms check fs.read       --path /home/jay/notes.md
/// cos perms check net.dial      --host api.github.com:443
/// cos perms check secret.read   --name openai/api-key
/// cos perms check ui.notify                            # no scope = wild
/// cos perms check fs.delete     --path /tmp/x --wild   # explicit wild
/// ```
///
/// Output is a JSON document with a `decision` field of `allow` or
/// `deny`. On deny, the [`Denial`](crate::caps::Denial) is embedded
/// alongside, so callers do not need a second round-trip to learn
/// why.
fn cmd_check(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos perms check <verb> [--path <p> | --host <h> | --name <n> | --wild]"
            .into());
    }
    let verb_str = &args[0];
    let verb = Verb::parse(verb_str)
        .ok_or_else(|| format!("unknown verb `{verb_str}` (see `cos perms verbs`)"))?;

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
            other => return Err(format!("unexpected arg: {other}")),
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
            // Splice the explicit `decision` discriminator at the top
            // so consumers can branch on one field.
            obj.as_object_mut()
                .map(|m| m.insert("decision".into(), Value::String("deny".into())));
            Ok(obj)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_requires_verb_arg() {
        let err = cmd_check(&[]).unwrap_err();
        assert!(err.contains("usage:"));
    }

    #[test]
    fn check_rejects_unknown_verb() {
        let err = cmd_check(&["fs.invalid".into()]).unwrap_err();
        assert!(err.contains("unknown verb"));
    }

    #[test]
    fn check_no_scope_defaults_to_wild_and_permissive_allows() {
        // Without COS_SESSION the default mode is permissive → allow.
        // Save/restore env to avoid polluting other tests.
        let prev = std::env::var("COS_SESSION").ok();
        std::env::remove_var("COS_SESSION");
        let v = cmd_check(&["ui.notify".into()]).unwrap();
        assert_eq!(v["decision"], "allow");
        if let Some(p) = prev {
            std::env::set_var("COS_SESSION", p);
        }
    }

    #[test]
    fn check_with_path_scope_encodes_into_response() {
        let prev = std::env::var("COS_SESSION").ok();
        std::env::remove_var("COS_SESSION");
        let v = cmd_check(&["fs.read".into(), "--path".into(), "/tmp/x".into()]).unwrap();
        assert_eq!(v["decision"], "allow");
        assert_eq!(v["verb"], "fs.read");
        assert_eq!(v["scope"]["kind"], "path");
        assert_eq!(v["scope"]["value"], "/tmp/x");
        if let Some(p) = prev {
            std::env::set_var("COS_SESSION", p);
        }
    }
}
