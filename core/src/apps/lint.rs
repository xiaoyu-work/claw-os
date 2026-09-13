//! Shared static App lint for directory installation and the CLI.

use std::path::Path;

use serde_json::{json, Value};

use super::App;

pub(crate) fn app_lint_violations(app: &App) -> Vec<Value> {
    let mut violations = scan_app_for_ai_imports(&app.dir);
    violations.extend(scan_mcp_block(app));
    violations
}

/// On-disk lint checks for an app's `mcp` block. The manifest parser
/// already enforces structural validity (duplicate tool names,
/// undeclared scope args, missing English text, etc.) and
/// `apps::discover` would have skipped the app otherwise. What we
/// still need to verify here is that the artefacts referenced by the
/// manifest exist on disk — most importantly the `mcp.entry` program,
/// since a missing entry breaks the agent at first call rather than at
/// install time.
fn scan_mcp_block(app: &App) -> Vec<Value> {
    let Some(service) = app.manifest.mcp.as_ref() else {
        return Vec::new();
    };
    let entry_rel = service
        .entry
        .clone()
        .unwrap_or_else(|| app.manifest.runtime.default_mcp_entry().to_string());
    let mut hits = Vec::new();
    if entry_rel.starts_with('/') {
        // An absolute entry is a claim on a system program, and only
        // the kernel's fixed native-desktop table may make it. Saying
        // so at lint time is the same answer the launcher gives, just
        // earlier.
        if crate::worker::trusted_desktop::allowlisted_system_program(&app.manifest.id)
            != Some(entry_rel.as_str())
        {
            hits.push(json!({
                "kind": "mcp.entry-not-allowlisted",
                "file": entry_rel,
                "hint": format!(
                    "Manifest names `{entry_rel}` outside its package. Only the kernel's \
                     fixed native-desktop table may name a system program, and it does not \
                     name this one for `{}`.",
                    app.manifest.id,
                ),
            }));
        }
        return hits;
    }
    let entry_abs = app.dir.join(&entry_rel);
    if !entry_abs.is_file() {
        hits.push(json!({
            "kind": "mcp.entry-missing",
            "file": entry_abs.display().to_string(),
            "hint": format!(
                "Manifest declares an `mcp` block with {} tool(s) but the entry script \
                 `{}` is not present on disk. The kernel agent will fail to bring up the MCP \
                 server on the first call.",
                service.tools.len(),
                entry_rel,
            ),
        }));
    }
    hits
}

/// Walk an app directory looking for `*.py` files that import one of
/// the forbidden provider SDKs. Returns a list of `{file, line, text}`
/// hits.
fn scan_app_for_ai_imports(app_dir: &Path) -> Vec<Value> {
    const FORBIDDEN: &[&str] = &[
        "openai",
        "anthropic",
        "google.generativeai",
        "vertexai",
        "cohere",
        "mistralai",
        "replicate",
        "boto3.client(\"bedrock",
        "boto3.client('bedrock",
    ];
    let mut hits = Vec::new();
    walk_py(app_dir, &mut |path, contents| {
        for (idx, line) in contents.lines().enumerate() {
            let trimmed = line.trim_start();
            if !(trimmed.starts_with("import ") || trimmed.starts_with("from ")) {
                // Allow grepping for the boto3-bedrock shape too.
                if !FORBIDDEN.iter().any(|f| trimmed.contains(f)) {
                    continue;
                }
            }
            for needle in FORBIDDEN {
                if trimmed.contains(needle)
                    && (trimmed.starts_with("import ")
                        || trimmed.starts_with("from ")
                        || trimmed.contains(".client"))
                {
                    hits.push(json!({
                        "file": path.display().to_string(),
                        "line": idx + 1,
                        "text": line.to_string(),
                        "matched": needle.to_string(),
                    }));
                    break;
                }
            }
        }
    });
    hits
}

fn walk_py(dir: &Path, f: &mut dyn FnMut(&Path, &str)) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            // Skip vendored / build / hidden directories.
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with('.') || name == "node_modules" || name == "__pycache__" {
                continue;
            }
            walk_py(&p, f);
        } else if p.extension().and_then(|e| e.to_str()) == Some("py") {
            if let Ok(contents) = std::fs::read_to_string(&p) {
                f(&p, &contents);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/apps/lint.rs"
    ));
}
