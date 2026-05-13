//! App manifest — declarative capability requirements.
//!
//! Every app on Claw OS ships an `app.json` in this shape. Apps speak
//! the same vocabulary as the kernel: each operation lists the
//! [`Cap`](super::cap::Cap)s it needs, and the kernel mediates every
//! invocation through that list. There is no implicit access path; if
//! an op doesn't declare a need, it cannot exercise the corresponding
//! verb.
//!
//! ## JSON shape (informal)
//!
//! ```jsonc
//! {
//!   "id": "fs",
//!   "version": "0.2.0",
//!   "name":    "Files",
//!   "summary": "Browse, read, write, and search files.",
//!   "icon":    "📁",
//!   "runtime": "python",
//!   "entry":   "main.py",
//!
//!   "operations": {
//!     "ls": {
//!       "label":   "List files",
//!       "summary": "Show the names of files inside a folder.",
//!       "args": [
//!         { "name": "path", "kind": "path", "required": true }
//!       ],
//!       "needs": [
//!         {
//!           "verb": "fs.meta",
//!           "scope": { "kind": "from-arg", "arg": "path" },
//!           "why":  "Read directory entries to list files."
//!         }
//!       ]
//!     },
//!
//!     "rm": {
//!       "label": "Delete a file",
//!       "args": [ { "name": "path", "kind": "path", "required": true } ],
//!       "needs": [
//!         {
//!           "verb": "fs.delete",
//!           "scope": { "kind": "from-arg", "arg": "path" },
//!           "why":  "Remove the file you specified."
//!         }
//!       ]
//!     }
//!   }
//! }
//! ```
//!
//! ## Key rules enforced by [`Manifest::validate`]
//!
//! - `id` is `[a-z][a-z0-9_-]*` and matches the directory name (the
//!   caller checks the latter).
//! - Every `name`, `label`, `summary`, and `why` must include at least
//!   an English translation.
//! - Every `verb` must be a recognised [`Verb`] (otherwise serde fails
//!   at parse time).
//! - Every `from-arg` scope must reference an arg the operation
//!   actually declares.
//! - `runtime` ∈ {python, node, shell, binary}.
//! - Authors must declare a scope explicitly. There is no implicit
//!   wildcard; `wild` is a separate variant authors opt into knowingly.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::i18n::LocalizedText;

use super::scope::Scope;
use super::verb::Verb;

// ---------------------------------------------------------------------------
// Top-level manifest
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub name: LocalizedText,
    #[serde(default)]
    pub summary: LocalizedText,
    #[serde(default)]
    pub icon: Option<String>,

    /// Which interpreter the bridge invokes for this app's entry point.
    #[serde(default)]
    pub runtime: Runtime,

    /// Path to the entry file *relative to the app directory*. If
    /// absent the bridge uses the runtime's default (`main.py`,
    /// `main.js`, `main.sh`, `main`).
    #[serde(default)]
    pub entry: Option<String>,

    /// Operations the app exposes, keyed by command name. The key is
    /// the verb the agent sees; the body describes its inputs and
    /// capability needs.
    #[serde(default)]
    pub operations: BTreeMap<String, Operation>,

    /// Free-form dependency declarations. Preserved for forward
    /// compatibility — the bridge's package resolver consumes this.
    #[serde(default)]
    pub dependencies: serde_json::Value,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    #[default]
    Python,
    Node,
    Shell,
    Binary,
}

impl Runtime {
    /// Default entry file for this runtime. Platform-aware: Windows
    /// gets `.bat` / `.exe` so packaged apps can ship a single
    /// manifest that works on every OS.
    pub fn default_entry(self) -> &'static str {
        match self {
            Runtime::Python => "main.py",
            Runtime::Node => "main.js",
            Runtime::Shell => {
                if cfg!(windows) {
                    "main.bat"
                } else {
                    "main.sh"
                }
            }
            Runtime::Binary => {
                if cfg!(windows) {
                    "main.exe"
                } else {
                    "main"
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub label: LocalizedText,
    #[serde(default)]
    pub summary: LocalizedText,
    /// Declared input parameters. Order is significant for the UI.
    #[serde(default)]
    pub args: Vec<Arg>,
    /// Capability requirements. Empty means the operation is purely
    /// local (no gated action); the kernel still records the call in
    /// the audit log but does not prompt for permission.
    #[serde(default)]
    pub needs: Vec<Need>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Arg {
    /// Identifier referenced by `from-arg` scope bindings.
    pub name: String,
    /// What kind of value this arg holds; the UI uses this to pick a
    /// widget and to validate the input.
    pub kind: ArgKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Human-readable help. Optional.
    #[serde(default)]
    pub label: LocalizedText,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ArgKind {
    /// Filesystem path. Matches [`Scope::Path`] when used in a scope.
    Path,
    /// `host[:port]`. Matches [`Scope::Host`].
    Host,
    /// Arbitrary named resource. Matches [`Scope::Name`].
    Name,
    /// Free-form text — not bindable to a scope.
    Text,
    /// Numeric input.
    Number,
    /// Boolean toggle.
    Bool,
}

impl ArgKind {
    /// Returns true if values of this kind can populate a [`Scope`].
    pub fn binds_to_scope(self) -> bool {
        matches!(self, ArgKind::Path | ArgKind::Host | ArgKind::Name)
    }
}

// ---------------------------------------------------------------------------
// Capability needs
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Need {
    pub verb: Verb,
    pub scope: ScopeBinding,
    /// Reason shown to the user in the approval dialog. Authors are
    /// expected to write this in plain language ("Read the file you
    /// asked me to summarise"), not jargon.
    pub why: LocalizedText,
}

/// How an operation's scope is determined at invocation time.
///
/// - [`ScopeBinding::FromArg`] — late binding: at call time the kernel
///   reads the named argument's value and constructs a [`Scope`]
///   matching the [`ArgKind`].
/// - [`ScopeBinding::Fixed`] — the scope is hard-coded in the manifest.
///   Useful for ops that always touch the same resource (e.g. a
///   per-app data directory).
/// - [`ScopeBinding::Wild`] — explicit wildcard. The author has to spell
///   this out; there is no implicit `*`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ScopeBinding {
    FromArg { arg: String },
    Fixed { scope: Scope },
    Wild,
}

// ---------------------------------------------------------------------------
// Parsing & validation
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid manifest id `{0}`: must match [a-z][a-z0-9_-]*")]
    InvalidId(String),
    #[error("operation `{op}`: arg `{arg}` declared twice")]
    DuplicateArg { op: String, arg: String },
    #[error("operation `{op}`: need #{idx} references undeclared arg `{arg}`")]
    NeedRefsUndeclaredArg { op: String, idx: usize, arg: String },
    #[error(
        "operation `{op}`: need #{idx} (verb `{verb}`) binds to arg `{arg}` of kind \
         `{kind:?}` which cannot populate a scope (expected path/host/name)"
    )]
    NeedArgKindMismatch {
        op: String,
        idx: usize,
        verb: String,
        arg: String,
        kind: ArgKind,
    },
    #[error("operation `{op}`: need #{idx}: {detail}")]
    NeedInvalid {
        op: String,
        idx: usize,
        detail: String,
    },
    #[error("operation `{op}`: {field}: {detail}")]
    LocalizedTextInvalid {
        op: String,
        field: &'static str,
        detail: String,
    },
    #[error("manifest field `{field}`: {detail}")]
    TopLevelTextInvalid {
        field: &'static str,
        detail: String,
    },
}

impl Manifest {
    /// Parse a manifest from JSON text.
    pub fn from_json(s: &str) -> Result<Self, ManifestError> {
        let m: Manifest = serde_json::from_str(s)?;
        m.validate()?;
        Ok(m)
    }

    /// Validate the manifest's invariants. Called automatically by
    /// [`from_json`].
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !is_valid_id(&self.id) {
            return Err(ManifestError::InvalidId(self.id.clone()));
        }
        self.name.validate().map_err(|d| ManifestError::TopLevelTextInvalid {
            field: "name",
            detail: d,
        })?;

        for (op_name, op) in &self.operations {
            op.label
                .validate()
                .map_err(|d| ManifestError::LocalizedTextInvalid {
                    op: op_name.clone(),
                    field: "label",
                    detail: d,
                })?;
            // Args must have unique names.
            let mut seen_args: BTreeMap<&str, &Arg> = BTreeMap::new();
            for arg in &op.args {
                if seen_args.insert(arg.name.as_str(), arg).is_some() {
                    return Err(ManifestError::DuplicateArg {
                        op: op_name.clone(),
                        arg: arg.name.clone(),
                    });
                }
            }
            // Needs must reference declared args and use compatible kinds.
            for (idx, need) in op.needs.iter().enumerate() {
                need.why
                    .validate()
                    .map_err(|d| ManifestError::NeedInvalid {
                        op: op_name.clone(),
                        idx,
                        detail: format!("why: {d}"),
                    })?;
                match &need.scope {
                    ScopeBinding::FromArg { arg } => {
                        let a = seen_args.get(arg.as_str()).ok_or_else(|| {
                            ManifestError::NeedRefsUndeclaredArg {
                                op: op_name.clone(),
                                idx,
                                arg: arg.clone(),
                            }
                        })?;
                        if !a.kind.binds_to_scope() {
                            return Err(ManifestError::NeedArgKindMismatch {
                                op: op_name.clone(),
                                idx,
                                verb: need.verb.as_str().to_string(),
                                arg: arg.clone(),
                                kind: a.kind,
                            });
                        }
                    }
                    ScopeBinding::Fixed { scope: _ } => {}
                    ScopeBinding::Wild => {}
                }
            }
        }
        Ok(())
    }

    /// Resolve the operation's needs into concrete [`Cap`](super::cap::Cap)s
    /// for a specific invocation.
    ///
    /// `op_name` selects which operation; `args` is a map of arg name
    /// → JSON-encoded value (the same shape the bridge already passes
    /// through). Unknown args produce `None`; needs that bind to a
    /// missing arg are reported in the error.
    pub fn resolve_needs(
        &self,
        op_name: &str,
        args: &BTreeMap<String, serde_json::Value>,
    ) -> Result<Vec<super::cap::Cap>, ManifestError> {
        let op = self.operations.get(op_name).ok_or_else(|| {
            ManifestError::NeedInvalid {
                op: op_name.to_string(),
                idx: 0,
                detail: "unknown operation".into(),
            }
        })?;
        let mut out = Vec::with_capacity(op.needs.len());
        for (idx, need) in op.needs.iter().enumerate() {
            let scope = match &need.scope {
                ScopeBinding::FromArg { arg } => {
                    let val = args.get(arg).ok_or_else(|| {
                        ManifestError::NeedInvalid {
                            op: op_name.to_string(),
                            idx,
                            detail: format!("arg `{arg}` not supplied at call time"),
                        }
                    })?;
                    let arg_decl = op
                        .args
                        .iter()
                        .find(|a| a.name == *arg)
                        .ok_or_else(|| ManifestError::NeedRefsUndeclaredArg {
                            op: op_name.to_string(),
                            idx,
                            arg: arg.clone(),
                        })?;
                    scope_from_arg_value(arg_decl.kind, val).ok_or_else(|| {
                        ManifestError::NeedInvalid {
                            op: op_name.to_string(),
                            idx,
                            detail: format!(
                                "arg `{arg}` value is not a {kind:?}",
                                kind = arg_decl.kind
                            ),
                        }
                    })?
                }
                ScopeBinding::Fixed { scope } => scope.clone(),
                ScopeBinding::Wild => Scope::Wild,
            };
            out.push(super::cap::Cap::new(need.verb, scope));
        }
        Ok(out)
    }
}

fn scope_from_arg_value(kind: ArgKind, value: &serde_json::Value) -> Option<Scope> {
    let s = value.as_str()?;
    Some(match kind {
        ArgKind::Path => Scope::path(s),
        ArgKind::Host => Scope::host(s),
        ArgKind::Name => Scope::name(s),
        _ => return None,
    })
}

fn is_valid_id(s: &str) -> bool {
    let mut bytes = s.bytes();
    match bytes.next() {
        Some(b) if b.is_ascii_lowercase() => {}
        _ => return false,
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Manifest {
        Manifest::from_json(s).expect("manifest should be valid")
    }

    #[test]
    fn minimal_manifest_parses() {
        let m = parse(
            r#"{
              "id": "fs",
              "version": "0.2.0",
              "name": "Files"
            }"#,
        );
        assert_eq!(m.id, "fs");
        assert_eq!(m.runtime, Runtime::Python);
        assert!(m.operations.is_empty());
    }

    #[test]
    fn invalid_id_rejected() {
        let err = Manifest::from_json(
            r#"{"id":"FS!","version":"0","name":"X"}"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::InvalidId(_)));
    }

    #[test]
    fn unknown_verb_rejected_at_parse_time() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "x": {
                  "label": "X",
                  "args": [],
                  "needs": [
                    {"verb": "fs.nonsense", "scope": {"kind":"wild"}, "why": "..."}
                  ]
                }
              }
            }"#,
        )
        .unwrap_err();
        // Serde error, not validate(): the unknown verb is caught at
        // deserialization time by Verb's manual impl.
        assert!(matches!(err, ManifestError::Json(_)));
    }

    #[test]
    fn need_referencing_undeclared_arg_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [],
                  "needs": [
                    {"verb": "fs.delete", "scope": {"kind":"from-arg","arg":"path"}, "why": "y"}
                  ]
                }
              }
            }"#,
        )
        .unwrap_err();
        match err {
            ManifestError::NeedRefsUndeclaredArg { op, idx, arg } => {
                assert_eq!(op, "rm");
                assert_eq!(idx, 0);
                assert_eq!(arg, "path");
            }
            other => panic!("expected NeedRefsUndeclaredArg, got {other:?}"),
        }
    }

    #[test]
    fn need_binding_to_text_arg_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [{"name": "path", "kind": "text"}],
                  "needs": [
                    {"verb": "fs.delete", "scope": {"kind":"from-arg","arg":"path"}, "why": "y"}
                  ]
                }
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::NeedArgKindMismatch { .. }));
    }

    #[test]
    fn duplicate_arg_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "x": {
                  "label": "X",
                  "args": [
                    {"name": "p", "kind": "path"},
                    {"name": "p", "kind": "path"}
                  ]
                }
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateArg { .. }));
    }

    #[test]
    fn missing_english_in_top_level_name_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": {"zh-CN": "文件"}
            }"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::TopLevelTextInvalid { field: "name", .. }));
    }

    #[test]
    fn missing_english_in_op_label_rejected() {
        let err = Manifest::from_json(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "ls": {
                  "label": {"zh-CN": "列表"}
                }
              }
            }"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ManifestError::LocalizedTextInvalid { field: "label", .. }
        ));
    }

    #[test]
    fn resolve_needs_substitutes_runtime_arg_value() {
        let m = parse(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [{"name": "path", "kind": "path", "required": true}],
                  "needs": [
                    {"verb": "fs.delete",
                     "scope": {"kind":"from-arg","arg":"path"},
                     "why": "Remove the file you specified."}
                  ]
                }
              }
            }"#,
        );
        let mut args = BTreeMap::new();
        args.insert("path".to_string(), serde_json::json!("/home/jay/x.md"));
        let caps = m.resolve_needs("rm", &args).unwrap();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].verb, Verb::FS_DELETE);
        assert_eq!(caps[0].scope, Scope::path("/home/jay/x.md"));
    }

    #[test]
    fn resolve_needs_with_fixed_scope() {
        let m = parse(
            r#"{
              "id": "log",
              "version": "0.1",
              "name": "Log",
              "operations": {
                "tail": {
                  "label": "Tail logs",
                  "needs": [
                    {"verb": "data.log.read",
                     "scope": {"kind":"fixed","scope":{"kind":"name","value":"system/*"}},
                     "why": "Read recent log lines."}
                  ]
                }
              }
            }"#,
        );
        let caps = m.resolve_needs("tail", &BTreeMap::new()).unwrap();
        assert_eq!(caps[0].verb, Verb::DATA_LOG_READ);
        assert_eq!(caps[0].scope, Scope::name("system/*"));
    }

    #[test]
    fn resolve_needs_missing_arg_at_runtime_is_error() {
        let m = parse(
            r#"{
              "id": "fs",
              "version": "0.1",
              "name": "Files",
              "operations": {
                "rm": {
                  "label": "Delete",
                  "args": [{"name": "path", "kind": "path", "required": true}],
                  "needs": [
                    {"verb": "fs.delete",
                     "scope": {"kind":"from-arg","arg":"path"},
                     "why": "Remove the file you specified."}
                  ]
                }
              }
            }"#,
        );
        let err = m.resolve_needs("rm", &BTreeMap::new()).unwrap_err();
        match err {
            ManifestError::NeedInvalid { op, detail, .. } => {
                assert_eq!(op, "rm");
                assert!(detail.contains("not supplied"));
            }
            other => panic!("expected NeedInvalid, got {other:?}"),
        }
    }

    #[test]
    fn runtime_default_is_python() {
        let m = parse(
            r#"{"id":"x","version":"0","name":"X"}"#,
        );
        assert_eq!(m.runtime, Runtime::Python);
        assert_eq!(m.runtime.default_entry(), "main.py");
    }

    #[test]
    fn full_example_round_trips() {
        let src = r#"{
          "id": "fs",
          "version": "0.2.0",
          "name": "Files",
          "summary": "Browse, read, write, and search files.",
          "icon": "📁",
          "runtime": "python",
          "entry": "main.py",
          "operations": {
            "ls": {
              "label": "List files",
              "summary": "Show the names of files inside a folder.",
              "args": [{"name":"path","kind":"path","required":true}],
              "needs": [
                {"verb":"fs.meta",
                 "scope":{"kind":"from-arg","arg":"path"},
                 "why":"Read directory entries to list files."}
              ]
            },
            "mv": {
              "label": "Move a file",
              "args": [
                {"name":"src","kind":"path","required":true},
                {"name":"dst","kind":"path","required":true}
              ],
              "needs": [
                {"verb":"fs.read",   "scope":{"kind":"from-arg","arg":"src"}, "why":"Read the source file."},
                {"verb":"fs.write",  "scope":{"kind":"from-arg","arg":"dst"}, "why":"Write to the destination."},
                {"verb":"fs.delete", "scope":{"kind":"from-arg","arg":"src"}, "why":"Remove the source after copying."}
              ]
            }
          }
        }"#;
        let m = Manifest::from_json(src).unwrap();
        let json = serde_json::to_string(&m).unwrap();
        let back = Manifest::from_json(&json).unwrap();
        assert_eq!(back.id, m.id);
        assert_eq!(back.operations.len(), m.operations.len());
        assert_eq!(back.operations["mv"].needs.len(), 3);
    }
}
