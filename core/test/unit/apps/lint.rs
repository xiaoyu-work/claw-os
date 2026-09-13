use super::*;

use crate::caps::manifest::Runtime;

fn mcp_app(dir: &Path, id: &str, runtime: Runtime, entry: Option<&str>) -> App {
    let body = json!({
        "schema_version": 2,
        "id": id,
        "version": "1.0.0",
        "name": {"en": "Lint fixture"},
        "runtime": runtime,
        "mcp": {
            "entry": entry,
            "tools": [{"name": format!("{id}.run"), "summary": {"en": "Run"}}],
        },
    });
    App {
        manifest: crate::apps::AppManifest::from_json(&body.to_string()).unwrap(),
        dir: dir.to_path_buf(),
        provenance: Err("static lint fixture".into()),
    }
}

#[test]
fn lint_preserves_provider_import_diagnostics_and_source_order() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("main.py");
    let cases = [
        ("import openai", "openai"),
        ("from anthropic import Anthropic", "anthropic"),
        ("import google.generativeai", "google.generativeai"),
        ("import vertexai", "vertexai"),
        ("import cohere", "cohere"),
        ("import mistralai", "mistralai"),
        ("import replicate", "replicate"),
        (
            "client = boto3.client(\"bedrock-runtime\")",
            "boto3.client(\"bedrock",
        ),
        (
            "client = boto3.client('bedrock-runtime')",
            "boto3.client('bedrock",
        ),
    ];
    std::fs::write(
        &file,
        cases
            .iter()
            .map(|(line, _)| *line)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    let expected: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(index, (text, matched))| {
            json!({
                "file": file.display().to_string(),
                "line": index + 1,
                "text": text,
                "matched": matched,
            })
        })
        .collect();
    assert_eq!(scan_app_for_ai_imports(root.path()), expected);
}

#[test]
fn lint_skips_generated_and_hidden_directories_but_reads_nested_python() {
    let root = tempfile::tempdir().unwrap();
    for name in [".hidden", "node_modules", "__pycache__", "visible"] {
        let directory = root.path().join(name);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("provider.py"), "import openai\n").unwrap();
    }
    std::fs::write(
        root.path().join("main.py"),
        "from claw_os_sdk import ai\n# import anthropic\n",
    )
    .unwrap();
    std::fs::write(root.path().join("notes.txt"), "import anthropic\n").unwrap();
    let hits = scan_app_for_ai_imports(root.path());
    assert_eq!(
        hits,
        vec![json!({
            "file": root.path().join("visible").join("provider.py").display().to_string(),
            "line": 1,
            "text": "import openai",
            "matched": "openai",
        })]
    );
}

#[test]
fn lint_checks_default_and_explicit_package_mcp_entries_without_execution() {
    for runtime in [
        Runtime::Python,
        Runtime::Node,
        Runtime::Shell,
        Runtime::Binary,
    ] {
        for entry in [None, Some("bin/service")] {
            let root = tempfile::tempdir().unwrap();
            let app = mcp_app(root.path(), "entry", runtime, entry);
            let path = root
                .path()
                .join(entry.unwrap_or(runtime.default_mcp_entry()));
            let hits = app_lint_violations(&app);
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0]["kind"], "mcp.entry-missing");
            assert_eq!(hits[0]["file"], path.display().to_string());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::create_dir(&path).unwrap();
            assert_eq!(app_lint_violations(&app), hits);
            std::fs::remove_dir(&path).unwrap();
            std::fs::write(&path, "this is not an executable entrypoint\n").unwrap();
            assert!(app_lint_violations(&app).is_empty());
        }
    }
}

#[test]
fn lint_uses_the_fixed_native_system_program_table() {
    let root = tempfile::tempdir().unwrap();
    for (id, entry, allowed) in [
        ("cosmic-files", "/usr/bin/cosmic-files", true),
        ("cosmic-files", "/usr/bin/cosmic-edit", false),
        ("other", "/usr/bin/cosmic-files", false),
    ] {
        let app = mcp_app(root.path(), id, Runtime::Binary, Some(entry));
        let hits = app_lint_violations(&app);
        if allowed {
            assert!(hits.is_empty());
        } else {
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0]["kind"], "mcp.entry-not-allowlisted");
            assert_eq!(hits[0]["file"], entry);
        }
        assert!(!app.is_verified(), "lint does not authenticate an App");
    }
}
