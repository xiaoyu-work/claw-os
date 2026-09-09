//! Source-kind regressions for immutable migrated App fixtures.

use std::path::Path;

use serde_json::{json, Value};

#[path = "../test/support/app_sources.rs"]
mod app_sources;

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn write_json(root: &Path, relative: &str, value: &Value) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    write_json(
        root.path(),
        "packaging/apps.lock.json",
        &json!({
            "version": 1, "revision": REVISION, "products": ["files", "storage"],
            "capabilities": ["document-engine", "storage-sdk", "ai-helpers"],
            "apps": ["fs", "doc", "storage-manager", "db", "kv", "summarize"],
        }),
    );
    let source = root.path().join("build/app-sources").join(REVISION);
    write_json(
        &source,
        "products/files/package.json",
        &json!({"apps": ["apps/fs"]}),
    );
    write_json(
        &source,
        "products/files/apps/fs/app.json",
        &json!({"id": "fs"}),
    );
    write_json(
        &source,
        "products/storage/package.json",
        &json!({"apps": ["apps/storage-manager"]}),
    );
    write_json(
        &source,
        "products/storage/apps/storage-manager/app.json",
        &json!({"id": "storage-manager"}),
    );
    write_json(
        &source,
        "capabilities/document-engine/package.json",
        &json!({
            "kind": "shared-capability-client", "apps": ["apps/doc"],
        }),
    );
    write_json(
        &source,
        "capabilities/document-engine/apps/doc/app.json",
        &json!({"id": "doc"}),
    );
    write_json(
        &source,
        "capabilities/storage-sdk/package.json",
        &json!({
            "kind": "shared-capability-client", "apps": ["apps/db", "apps/kv"],
        }),
    );
    write_json(
        &source,
        "capabilities/storage-sdk/apps/db/app.json",
        &json!({"id": "db"}),
    );
    write_json(
        &source,
        "capabilities/storage-sdk/apps/kv/app.json",
        &json!({"id": "kv"}),
    );
    write_json(
        &source,
        "capabilities/ai-helpers/package.json",
        &json!({"kind": "shared-capability-client", "apps": ["apps/summarize"]}),
    );
    write_json(
        &source,
        "capabilities/ai-helpers/apps/summarize/app.json",
        &json!({"id": "summarize"}),
    );
    write_json(
        root.path(),
        "apps/summarize/app.json",
        &json!({"id": "summarize", "stale": true}),
    );
    write_json(
        root.path(),
        "apps/doc/app.json",
        &json!({"id": "doc", "stale": true}),
    );
    write_json(
        root.path(),
        "apps/db/app.json",
        &json!({"id": "db", "stale": true}),
    );
    write_json(
        root.path(),
        "apps/kv/app.json",
        &json!({"id": "kv", "stale": true}),
    );
    root
}

#[test]
fn declared_capability_and_product_resolve_only_their_locked_roots() {
    let root = fixture();
    let source = root.path().join("build/app-sources").join(REVISION);
    assert_eq!(
        app_sources::app_dir_in(root.path(), "doc"),
        source.join("capabilities/document-engine/apps/doc")
    );
    assert_eq!(
        app_sources::app_dir_in(root.path(), "fs"),
        source.join("products/files/apps/fs")
    );
    assert_eq!(
        app_sources::app_dir_in(root.path(), "db"),
        source.join("capabilities/storage-sdk/apps/db")
    );
    assert_eq!(
        app_sources::app_dir_in(root.path(), "kv"),
        source.join("capabilities/storage-sdk/apps/kv")
    );
    assert_eq!(
        app_sources::app_dir_in(root.path(), "summarize"),
        source.join("capabilities/ai-helpers/apps/summarize")
    );
    assert_eq!(
        app_sources::app_dir_in(root.path(), "storage-manager"),
        source.join("products/storage/apps/storage-manager")
    );
}

#[test]
fn missing_locked_capability_never_uses_the_local_os_copy() {
    for (group, app) in [
        ("document-engine", "doc"),
        ("storage-sdk", "db"),
        ("storage-sdk", "kv"),
        ("ai-helpers", "summarize"),
    ] {
        for missing in ["package.json".to_string(), format!("apps/{app}/app.json")] {
            let root = fixture();
            std::fs::remove_file(root.path().join(format!(
                "build/app-sources/{REVISION}/capabilities/{group}/{missing}"
            )))
            .unwrap();
            assert!(root.path().join(format!("apps/{app}/app.json")).exists());
            assert!(
                std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), app)).is_err()
            );
        }
    }
}

#[test]
fn capability_source_kind_is_explicit() {
    let root = fixture();
    let source = root.path().join("build/app-sources").join(REVISION);
    write_json(
        &source,
        "capabilities/document-engine/package.json",
        &json!({
            "kind": "product", "apps": ["apps/doc"],
        }),
    );
    assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
}

#[test]
fn lock_rejects_duplicate_traversing_or_nonimmutable_source_declarations() {
    let root = fixture();
    let path = root.path().join("packaging/apps.lock.json");
    let baseline: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for (field, value) in [
        ("capabilities", json!(["files"])),
        (
            "capabilities",
            json!(["document-engine", "document-engine"]),
        ),
        ("capabilities", json!(["../document-engine"])),
        ("apps", json!(["doc", "doc"])),
        ("revision", json!("main")),
    ] {
        let mut lock = baseline.clone();
        lock[field] = value;
        write_json(root.path(), "packaging/apps.lock.json", &lock);
        assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
    }
}

#[test]
fn missing_duplicate_or_traversing_app_layout_never_uses_local_source() {
    let root = fixture();
    let source = root.path().join("build/app-sources").join(REVISION);
    for apps in [
        json!([]),
        json!(["apps/doc", "apps/doc"]),
        json!(["apps/../doc"]),
        json!(["apps/missing"]),
    ] {
        write_json(
            &source,
            "capabilities/document-engine/package.json",
            &json!({
                "kind": "shared-capability-client", "apps": apps,
            }),
        );
        assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
    }
}

#[test]
fn capability_cannot_add_native_product_assets() {
    let root = fixture();
    let source = root.path().join("build/app-sources").join(REVISION);
    write_json(
        &source,
        "capabilities/document-engine/package.json",
        &json!({
            "kind": "shared-capability-client", "apps": ["apps/doc"], "native": {},
        }),
    );
    assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_capability_source_cannot_escape_its_declared_kind() {
    let root = fixture();
    let group = root.path().join(format!(
        "build/app-sources/{REVISION}/capabilities/document-engine"
    ));
    let outside = root.path().join("outside");
    std::fs::rename(&group, &outside).unwrap();
    std::os::unix::fs::symlink(outside, group).unwrap();
    assert!(std::panic::catch_unwind(|| app_sources::app_dir_in(root.path(), "doc")).is_err());
}

#[test]
fn version_one_product_only_locks_remain_supported() {
    let root = fixture();
    write_json(
        root.path(),
        "packaging/apps.lock.json",
        &json!({
            "version": 1, "revision": REVISION, "products": ["files"], "apps": ["fs"],
        }),
    );
    assert!(app_sources::app_dir_in(root.path(), "fs").ends_with("products/files/apps/fs"));
    assert_eq!(
        app_sources::app_dir_in(root.path(), "doc"),
        root.path().join("apps/doc")
    );
}

#[test]
fn published_doc_fixture_preserves_its_manifest_and_six_operations() {
    let directory = app_sources::app_dir("doc");
    assert!(directory.ends_with("capabilities/document-engine/apps/doc"));
    let manifest = cos::caps::manifest::Manifest::from_json(
        &std::fs::read_to_string(directory.join("app.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.id, "doc");
    assert_eq!(manifest.operations.len(), 6);
    assert!(manifest.operations.contains_key("read"));
    assert!(manifest.operations.contains_key("info"));
    assert!(manifest.operations.contains_key("convert"));
    assert!(manifest.operations.contains_key("summarize"));
    assert!(manifest.operations.contains_key("explain"));
    assert!(manifest.operations.contains_key("rewrite"));
    assert!(directory.join("main.py").is_file());
    assert!(directory.join("server.py").is_file());
}

#[test]
fn published_db_fixture_preserves_mcp_cli_arguments_and_exact_database_scopes() {
    use std::collections::BTreeMap;

    use cos::caps::{Cap, Scope, Verb};

    let directory = app_sources::app_dir("db");
    assert!(directory.ends_with("capabilities/storage-sdk/apps/db"));
    let manifest = cos::caps::manifest::Manifest::from_json(
        &std::fs::read_to_string(directory.join("app.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.id, "db");
    assert!(cos::apps::is_mcp_only_cli(&manifest));
    let service = manifest.mcp.as_ref().unwrap();
    let entry = service
        .entry
        .as_deref()
        .unwrap_or_else(|| manifest.runtime.default_mcp_entry());
    assert!(directory.join(entry).is_file());
    assert!(service.access.system_agent);
    assert!(!service.access.external_agents);
    assert_eq!(
        service
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        [
            "db.query",
            "db.exec",
            "db.tables",
            "db.schema",
            "db.databases"
        ]
    );
    let paths = cos::caps::args::PathContext {
        home: directory.clone(),
        cwd: None,
    };
    for (command, argv, expected, verb, scope) in [
        (
            "query",
            vec!["personal", "SELECT * FROM items"],
            json!({"database": "personal", "sql": "SELECT * FROM items"}),
            Verb::DATA_DB_READ,
            Scope::name("personal"),
        ),
        (
            "exec",
            vec!["personal", "CREATE TABLE items (id)"],
            json!({"database": "personal", "sql": "CREATE TABLE items (id)"}),
            Verb::DATA_DB_WRITE,
            Scope::name("personal"),
        ),
        (
            "tables",
            vec!["personal"],
            json!({"database": "personal"}),
            Verb::DATA_DB_READ,
            Scope::name("personal"),
        ),
        (
            "schema",
            vec!["personal", "items"],
            json!({"database": "personal", "table": "items"}),
            Verb::DATA_DB_READ,
            Scope::name("personal"),
        ),
        (
            "databases",
            vec![],
            json!({}),
            Verb::DATA_DB_READ,
            Scope::Wild,
        ),
    ] {
        let tool = cos::apps::mcp_tool_for_command(&manifest, command).unwrap();
        let schema = cos::apps::tool_schema(tool);
        assert_eq!(schema["stdin"], false);
        for argument in schema["parameters"].as_array().unwrap() {
            assert_eq!(argument["binding"], "positional");
            assert_eq!(argument["required"], true);
            assert_eq!(argument["type"], "string");
        }

        let argv = argv.into_iter().map(String::from).collect::<Vec<_>>();
        let supplied = cos::caps::args::bind_supplied_cli_args(&tool.args, &argv).unwrap();
        let expected: BTreeMap<String, Value> = serde_json::from_value(expected).unwrap();
        assert_eq!(supplied, expected);
        let call = manifest
            .resolve_mcp_tool_call(&tool.name, &supplied, &paths)
            .unwrap();
        assert_eq!(call.values, expected);
        assert_eq!(
            call.needs.into_iter().flatten().collect::<Vec<_>>(),
            vec![Cap::new(verb, scope)]
        );
    }
    for arguments in [
        json!({"database": "personal"}),
        json!({"database": 42, "sql": "SELECT 1"}),
        json!({"database": "personal", "sql": "SELECT 1", "session_id": "forged"}),
    ] {
        let supplied = serde_json::from_value(arguments).unwrap();
        assert!(manifest
            .resolve_mcp_tool_call("db.query", &supplied, &paths)
            .is_err());
    }
    assert!(cos::apps::mcp_tool_for_command(&manifest, "execute").is_err());
}

#[test]
fn published_net_fixture_preserves_mcp_bindings_and_exact_destination_scopes() {
    use cos::caps::{Cap, Scope, Verb};

    let directory = app_sources::app_dir("net");
    assert!(directory.ends_with("capabilities/http/apps/net"));
    let manifest = cos::caps::manifest::Manifest::from_json(
        &std::fs::read_to_string(directory.join("app.json")).unwrap(),
    )
    .unwrap();
    assert!(cos::apps::is_mcp_only_cli(&manifest));
    let service = manifest.mcp.as_ref().unwrap();
    assert!(service.access.system_agent);
    assert!(!service.access.external_agents);
    assert_eq!(
        service
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["net.fetch", "net.download"],
    );
    let entry = service
        .entry
        .as_deref()
        .unwrap_or_else(|| manifest.runtime.default_mcp_entry());
    assert!(directory.join(entry).is_file());
    let paths = cos::caps::args::PathContext {
        home: directory.clone(),
        cwd: None,
    };
    let fetch = cos::apps::mcp_tool_for_command(&manifest, "fetch").unwrap();
    let supplied = cos::caps::args::bind_supplied_cli_args(
        &fetch.args,
        &[
            "https://exam\u{ad}ple.test:/resource".into(),
            "--header".into(),
            "X-First: one".into(),
            "--header=X-Second: two".into(),
        ],
    )
    .unwrap();
    let call = manifest
        .resolve_mcp_tool_call(&fetch.name, &supplied, &paths)
        .unwrap();
    assert_eq!(call.values["url"], "https://example.test/resource");
    assert_eq!(
        call.values["header"],
        json!(["X-First: one", "X-Second: two"])
    );
    assert_eq!(call.values["method"], "GET");
    assert_eq!(call.values["timeout"], 30);
    assert_eq!(
        call.needs.into_iter().flatten().collect::<Vec<_>>(),
        [Cap::new(Verb::NET_DIAL, Scope::host("example.test:443"))]
    );
    let output = tempfile::tempdir().unwrap();
    let destination = output.path().join("download.bin");
    let download = cos::apps::mcp_tool_for_command(&manifest, "download").unwrap();
    let schema = cos::apps::tool_schema(download);
    assert_eq!(schema["parameters"][1]["required"], true);
    assert_eq!(schema["parameters"][1]["binding"], "positional");
    assert_eq!(schema["stdin"], false);
    let call = manifest
        .resolve_mcp_tool_call(
            &download.name,
            &serde_json::from_value(json!({
                "url": "http://example.test:8080/resource", "output": destination,
            }))
            .unwrap(),
            &paths,
        )
        .unwrap();
    assert_eq!(
        call.needs.into_iter().flatten().collect::<Vec<_>>(),
        [
            Cap::new(Verb::NET_DIAL, Scope::host("example.test:8080")),
            Cap::new(Verb::FS_WRITE, Scope::path(destination.to_string_lossy())),
        ]
    );
    assert!(manifest
        .resolve_mcp_tool_call(
            &download.name,
            &serde_json::from_value(json!({
                "url": "https://example.test/resource",
            }))
            .unwrap(),
            &paths
        )
        .is_err());
    assert!(manifest
        .resolve_mcp_tool_call(
            &fetch.name,
            &serde_json::from_value(json!({
                "url": "https://example.test/resource", "method": "PATCH",
            }))
            .unwrap(),
            &paths
        )
        .is_err());
}

#[test]
fn published_kv_fixture_preserves_cli_defaults_and_separate_key_and_store_scopes() {
    use cos::caps::manifest::ScopeBinding;
    use cos::caps::{Cap, Scope, Verb};

    let directory = app_sources::app_dir("kv");
    assert!(directory.ends_with("capabilities/storage-sdk/apps/kv"));
    let manifest = cos::caps::manifest::Manifest::from_json(
        &std::fs::read_to_string(directory.join("app.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.id, "kv");
    assert!(cos::apps::is_mcp_only_cli(&manifest));
    let service = manifest.mcp.as_ref().unwrap();
    let entry = service
        .entry
        .as_deref()
        .unwrap_or_else(|| manifest.runtime.default_mcp_entry());
    assert!(directory.join(entry).is_file());
    assert!(service.access.system_agent);
    assert!(!service.access.external_agents);
    assert_eq!(
        service
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["kv.get", "kv.set", "kv.del", "kv.list", "kv.dump"]
    );
    let paths = cos::caps::args::PathContext {
        home: directory.clone(),
        cwd: None,
    };
    for (command, argv, values, required) in [
        (
            "get",
            vec!["alpha"],
            json!({"key": "alpha"}),
            Cap::new(Verb::DATA_KV_READ, Scope::name("alpha")),
        ),
        (
            "set",
            vec!["alpha", "value"],
            json!({"key": "alpha", "value": "value"}),
            Cap::new(Verb::DATA_KV_WRITE, Scope::name("alpha")),
        ),
        (
            "del",
            vec!["alpha"],
            json!({"key": "alpha"}),
            Cap::new(Verb::DATA_KV_DELETE, Scope::name("alpha")),
        ),
        (
            "list",
            vec![],
            json!({"pattern": "*"}),
            Cap::new(Verb::DATA_KV_READ, Scope::Wild),
        ),
        (
            "list",
            vec!["alpha*"],
            json!({"pattern": "alpha*"}),
            Cap::new(Verb::DATA_KV_READ, Scope::Wild),
        ),
        (
            "dump",
            vec![],
            json!({}),
            Cap::new(Verb::DATA_KV_READ, Scope::Wild),
        ),
    ] {
        let tool = cos::apps::mcp_tool_for_command(&manifest, command).unwrap();
        let argv = argv.into_iter().map(String::from).collect::<Vec<_>>();
        let supplied = cos::caps::args::bind_supplied_cli_args(&tool.args, &argv).unwrap();
        let effective = manifest
            .resolve_mcp_tool_call(&tool.name, &supplied, &paths)
            .unwrap();
        assert_eq!(serde_json::to_value(effective.values).unwrap(), values);
        assert_eq!(
            effective.needs.into_iter().flatten().collect::<Vec<_>>(),
            [required]
        );
        if matches!(command, "list" | "dump") {
            assert!(matches!(
                tool.needs[0].scope,
                ScopeBinding::Fixed { scope: Scope::Wild }
            ));
        }
    }
    for (tool, arguments) in [
        ("kv.get", json!({})),
        ("kv.get", json!({"key": 42})),
        ("kv.set", json!({"key": "alpha", "value": false})),
        ("kv.del", json!({"key": "alpha", "session_id": "forged"})),
        ("kv.list", json!({"pattern": []})),
        ("kv.dump", json!({"key": "extra"})),
    ] {
        let supplied = serde_json::from_value(arguments).unwrap();
        assert!(manifest
            .resolve_mcp_tool_call(tool, &supplied, &paths)
            .is_err());
    }
    assert!(cos::apps::mcp_tool_for_command(&manifest, "delete").is_err());
}

#[test]
fn published_summarize_preserves_cli_ai_consent_and_independent_memory_contract() {
    use cos::caps::manifest::{AiPolicy, Manifest};
    use cos::caps::{Cap, Scope, Verb};

    let directory = app_sources::app_dir("summarize");
    assert!(directory.ends_with("capabilities/ai-helpers/apps/summarize"));
    let manifest =
        Manifest::from_json(&std::fs::read_to_string(directory.join("app.json")).unwrap()).unwrap();
    assert_eq!(manifest.id, "summarize");
    assert!(cos::apps::is_mcp_only_cli(&manifest));
    let service = manifest.mcp.as_ref().unwrap();
    let entry = service
        .entry
        .as_deref()
        .unwrap_or_else(|| manifest.runtime.default_mcp_entry());
    assert!(directory.join(entry).is_file());
    assert!(service.access.system_agent);
    assert!(!service.access.external_agents);
    assert_eq!(service.tools.len(), 1);
    let tool = cos::apps::mcp_tool_for_command(&manifest, "run").unwrap();
    assert_eq!(tool.name, "summarize.run");
    assert_eq!(cos::apps::tool_schema(tool)["stdin"], false);
    let supplied = cos::caps::args::bind_supplied_cli_args(
        &tool.args,
        &["explicit external text".to_string()],
    )
    .unwrap();
    let paths = cos::caps::args::PathContext {
        home: directory,
        cwd: None,
    };
    let call = manifest
        .resolve_mcp_tool_call(&tool.name, &supplied, &paths)
        .unwrap();
    assert_eq!(call.values, supplied);
    assert_eq!(
        call.needs.into_iter().flatten().collect::<Vec<_>>(),
        [
            Cap::new(Verb::AI_CHAT_UNTRUSTED, Scope::Wild),
            Cap::new(Verb::MEMORY_WRITE, Scope::self_ref("summarize")),
        ]
    );
    let original: AiPolicy = serde_json::from_value(json!({
        "budget": {"monthly_units": 100000},
        "safety": "strict", "origins": ["external-content"],
    }))
    .unwrap();
    let consent = cos::ai::consent::Consent::approve(original.clone());
    assert_eq!(manifest.ai.as_ref().unwrap(), &original);
    assert_eq!(
        cos::ai::consent::freshness(manifest.ai.as_ref().unwrap(), &consent),
        cos::ai::consent::Freshness::Fresh
    );
    for value in [
        json!({}),
        json!({"text": false}),
        json!({"text": "input", "path": "/not-read"}),
        json!({"text": "input", "app_id": "other-app"}),
        json!({"text": "input", "model": "caller-model"}),
        json!({"text": "input", "origin": "trusted"}),
        json!({"text": "input", "max_units": 1}),
        json!({"text": "input", "session_id": "forged"}),
    ] {
        let arguments = serde_json::from_value(value).unwrap();
        assert!(manifest
            .resolve_mcp_tool_call(&tool.name, &arguments, &paths)
            .is_err());
    }
}
