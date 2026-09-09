"""Pinned external App inputs must not create an unsigned runtime fallback."""

import fnmatch
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys

import pytest

from test_support import authenticated_mcp_params, mcp_process


ROOT = Path(__file__).resolve().parents[3]


@pytest.mark.parametrize("service", [
    "claw-os-app-permissions-v1", "claw-os-capture-v1", "claw-os-media-player-v1", "claw-os-notifications-v1",
])
def test_brokered_desktop_service_is_an_installed_dependency(service):
    agent = (ROOT / "packaging/deb/claw-os-agent/control").read_text()
    desktop = (ROOT / "packaging/deb/claw-os-desktop/control").read_text()
    assert service in next(line for line in agent.splitlines() if line.startswith("Provides:"))
    assert service in next(line for line in desktop.splitlines() if line.startswith("Depends:"))

SPEC = importlib.util.spec_from_file_location("app_sources", ROOT / "scripts" / "app_sources.py")
sources = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sources)


@pytest.fixture
def locked_source(tmp_path, monkeypatch):
    upstream = tmp_path / "upstream"
    upstream.mkdir()
    git = ["git", "-C", str(upstream)]
    subprocess.run([*git, "init", "--quiet"], check=True)
    subprocess.run([*git, "config", "user.name", "Fixture"], check=True)
    subprocess.run([*git, "config", "user.email", "fixture@example.invalid"], check=True)
    (upstream / "tools").mkdir()
    (upstream / "tools" / "stage.py").write_text(
        'import pathlib, sys\n'
        'root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])\n'
        '(root / "usr/lib/cos/apps/mail-ai").mkdir(parents=True)\n'
        'print(\'["mail-ai"]\')\n'
    )
    app = upstream / "products/mail/apps/mail-ai"
    app.mkdir(parents=True)
    (app / "app.json").write_text('{"id":"mail-ai"}')
    (upstream / "products/mail/package.json").write_text('{"apps":["apps/mail-ai"]}')
    subprocess.run([*git, "add", "tools/stage.py", "products/mail"], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", "fixture"], check=True)
    revision = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    root = tmp_path / "os"
    (root / "packaging").mkdir(parents=True)
    lock = {"version": 1, "repository": str(upstream), "revision": revision,
            "products": ["mail"], "apps": ["mail-ai"]}
    (root / "packaging" / "apps.lock.json").write_text(json.dumps(lock))
    monkeypatch.setattr(sources, "ROOT", root)
    return root, lock


def test_staging_uses_exact_locked_source(locked_source, tmp_path):
    _, lock = locked_source
    target = tmp_path / "stage"
    assert sources.stage_products(target) == ["mail-ai"]
    assert (target / "usr/lib/cos/apps/mail-ai").is_dir()
    cached = sources.prepare_sources(lock)
    assert subprocess.check_output(
        ["git", "-C", str(cached), "rev-parse", "HEAD"], text=True
    ).strip() == lock["revision"]


def test_modified_cached_source_is_rejected(locked_source):
    _, lock = locked_source
    cached = sources.prepare_sources(lock)
    (cached / "tools" / "stage.py").write_text("raise RuntimeError('modified')\n")
    with pytest.raises(RuntimeError, match="modified"):
        sources.prepare_sources(lock)


def test_moving_branch_is_not_a_valid_pin(locked_source):
    root, lock = locked_source
    lock["revision"] = "main"
    (root / "packaging" / "apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(ValueError, match="full Git revision"):
        sources.read_lock()


@pytest.mark.parametrize("value", ["mail", [], [{}], ["mail", "mail"], ["../mail"]])
def test_invalid_product_list_is_rejected(locked_source, value):
    root, lock = locked_source
    lock["products"] = value
    (root / "packaging" / "apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(ValueError, match="products"):
        sources.read_lock()


@pytest.mark.parametrize("value", [
    "document-engine", [{}], ["document-engine", "document-engine"], ["../document-engine"],
])
def test_invalid_capability_list_is_rejected(locked_source, value):
    root, lock = locked_source
    lock["capabilities"] = value
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(ValueError, match="capabilities"):
        sources.read_lock()


def test_optional_capabilities_preserve_version_one_product_locks(locked_source):
    root, lock = locked_source
    assert sources.declared_sources(sources.read_lock()) == [("product", "mail")]
    lock["capabilities"] = []
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    assert sources.declared_sources(sources.read_lock()) == [("product", "mail")]


def test_duplicate_group_name_across_kinds_is_rejected(locked_source):
    root, lock = locked_source
    lock["capabilities"] = ["mail"]
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(ValueError, match="Duplicate source group"):
        sources.read_lock()


@pytest.fixture(params=[
    ("document-engine", "doc"), ("storage-sdk", "db"), ("storage-sdk", "kv"), ("http", "net"),
])
def capability_source(locked_source, request):
    root, lock = locked_source
    upstream = Path(lock["repository"])
    group_name, app_id = request.param
    group = upstream / "capabilities" / group_name
    app = group / "apps" / app_id
    app.mkdir(parents=True)
    (group / "package.json").write_text(json.dumps({
        "kind": "shared-capability-client", "apps": [f"apps/{app_id}"],
    }))
    (app / "app.json").write_text(json.dumps({"id": app_id}))
    (upstream / "tools/stage.py").write_text(
        'import json, pathlib, shutil, sys\n'
        'kind = sys.argv[sys.argv.index("--kind") + 1] if "--kind" in sys.argv else "product"\n'
        'source = pathlib.Path(__file__).parents[1] / {"product":"products","capability":"capabilities"}[kind] / sys.argv[1]\n'
        'root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])\n'
        'selected = sys.argv[sys.argv.index("--apps") + 1:] if "--apps" in sys.argv else None\n'
        'installed = []\n'
        'for relative in json.loads((source / "package.json").read_text())["apps"]:\n'
        '    app = source / relative\n'
        '    identity = json.loads((app / "app.json").read_text())["id"]\n'
        '    if selected is None or identity in selected:\n'
        '        shutil.copytree(app, root / "usr/lib/cos/apps" / app.name)\n'
        '        installed.append(identity)\n'
        'print(json.dumps(installed))\n'
    )
    git = ["git", "-C", str(upstream)]
    subprocess.run([*git, "add", f"capabilities/{group_name}", "tools/stage.py"], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", "capability fixture"], check=True)
    lock["revision"] = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    lock["capabilities"] = [group_name]
    lock["apps"].append(app_id)
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    partition = root / "packaging/deb/claw-os-desktop/apps.list"
    partition.parent.mkdir(parents=True)
    partition.write_text("desktop-fixture\n")
    return root, lock


def test_capability_uses_its_explicit_immutable_root_and_agent_partition(capability_source, tmp_path):
    root, lock = capability_source
    group, app_id = lock["capabilities"][0], lock["apps"][-1]
    assert sources.declared_sources(lock) == [("product", "mail"), ("capability", group)]
    assert sources.stage_products(tmp_path / "all") == ["mail-ai", app_id]
    assert sources.stage_products(tmp_path / "agent", "agent") == ["mail-ai", app_id]
    assert sources.stage_products(tmp_path / "desktop", "desktop") == []
    assert sources.app_path(app_id) == (
        root / "build/app-sources" / lock["revision"] / "capabilities" / group / "apps" / app_id
    )
    assert not (tmp_path / "desktop/usr/lib/cos/apps" / app_id).exists()


def test_missing_capability_never_uses_local_os_or_product_source(capability_source, monkeypatch):
    root, lock = capability_source
    group, app_id = lock["capabilities"][0], lock["apps"][-1]
    source = sources.prepare_sources(lock)
    for alternate in (root / "apps" / app_id, source / "products" / group / "apps" / app_id):
        alternate.mkdir(parents=True)
        (alternate / "app.json").write_text(json.dumps({"id": app_id}))
    (source / "capabilities" / group / "package.json").unlink()
    monkeypatch.setattr(sources, "prepare_sources", lambda _: source)
    with pytest.raises(RuntimeError, match="Locked capability source package is missing"):
        sources.app_path(app_id)


@pytest.mark.parametrize("change", ["kind", "duplicate", "traversal", "missing", "native", "dependency"])
def test_capability_contract_errors_fail_before_staging(capability_source, monkeypatch, tmp_path, change):
    _, lock = capability_source
    group, app_id = lock["capabilities"][0], lock["apps"][-1]
    source = sources.prepare_sources(lock)
    path = source / "capabilities" / group / "package.json"
    package = json.loads(path.read_text())
    if change == "kind":
        package["kind"] = "product"
    elif change == "duplicate":
        package["apps"].append(f"apps/{app_id}")
    elif change == "traversal":
        package["apps"] = [f"apps/../{app_id}"]
    elif change == "missing":
        package["apps"] = ["apps/missing"]
    elif change == "native":
        package["native"] = {}
    else:
        package["python_dependencies"] = [
            {"kind": "product", "name": "unlocked", "library": "claw_files", "apps": [app_id]},
        ]
    path.write_text(json.dumps(package))
    monkeypatch.setattr(sources, "prepare_sources", lambda _: source)
    with pytest.raises((ValueError, RuntimeError), match="kind|Duplicate|layout|missing|native|locked source"):
        sources.stage_products(tmp_path / "stage")
    assert not (tmp_path / "stage").exists()


def test_native_preparation_does_not_invent_capability_components(capability_source):
    _, lock = capability_source
    destination = sources.prepare_native()
    assert (destination / "revision").read_text().strip() == lock["revision"]
    assert json.loads((destination / "native-libraries.json").read_text())["libraries"] == {}
    assert not (destination / lock["capabilities"][0]).exists()
    assert not (destination / "doc").exists()


def test_unexpected_installed_identity_is_rejected(locked_source, tmp_path):
    root, lock = locked_source
    lock["apps"] = ["unexpected"]
    (root / "packaging" / "apps.lock.json").write_text(json.dumps(lock))
    with pytest.raises(RuntimeError, match="identities"):
        sources.stage_products(tmp_path / "stage")


def test_agent_package_keeps_native_authority_in_os():
    script = (ROOT / "packaging/deb/build-debs.sh").read_text()
    assert '"$PROJECT_DIR/scripts/app_sources.py" --stage "$AGENT_STAGE"' in script
    assert '$AGENT_STAGE/usr/lib/cos/claw-mail-ai-host' in script
    assert '"$PROJECT_DIR/extensions/claw-mail-ai"' not in script


def test_browser_installer_resolves_both_sources_from_the_os_pin(locked_source):
    root, lock = locked_source
    upstream = Path(lock["repository"])
    for relative in (
        "products/browser/extension/manifest.json",
        "products/browser/apps/browser-attached/native_host.py",
    ):
        path = upstream / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("fixture\n")
    git = ["git", "-C", str(upstream)]
    subprocess.run([*git, "add", "products/browser"], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", "browser fixture"], check=True)
    lock["revision"] = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    lock["products"] = ["browser"]
    lock["apps"] = ["browser-attached"]
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    (root / "scripts").mkdir()
    shutil.copyfile(ROOT / "scripts/app_sources.py", root / "scripts/app_sources.py")
    script = (ROOT / "tools/install-browser-agent.sh").read_text()
    block = script.split('APP_SOURCES=', 1)[1].split('\nEXT_DEST=', 1)[0]
    result = subprocess.run(
        ["bash", "-euc", 'APP_SOURCES=' + block + '\nprintf "%s\\n" "$EXT_SRC" "$APP_SRC"'],
        check=True, capture_output=True, text=True,
        env={**os.environ, "REPO_ROOT": str(root)},
    )
    cached = root / "build/app-sources" / lock["revision"]
    assert result.stdout.splitlines() == [
        str(cached / "products/browser/extension"),
        str(cached / "products/browser/apps/browser-attached"),
    ]
    assert (cached / "products/browser/extension/manifest.json").is_file()
    assert (cached / "products/browser/apps/browser-attached/native_host.py").is_file()


def test_browser_installer_copies_extension_assets_without_development_tests(tmp_path):
    source = tmp_path / "extension"
    source.mkdir()
    assets = {"manifest.json", "background.js", "content.js", "popup.html", "popup.js", "README.md"}
    for name in assets | {"test_contract.py"}:
        (source / name).write_text(name)
    target = tmp_path / "installed"
    script = (ROOT / "tools/install-browser-agent.sh").read_text()
    start = script.index('install -d -m 0755 "${EXT_DEST}"')
    end = script.index('echo "[claw] installing native host')
    subprocess.run(["bash", "-euc", script[start:end]], check=True, env={
        **os.environ, "EXT_SRC": str(source), "EXT_DEST": str(target),
    })
    assert {path.name for path in target.iterdir()} == assets
    for path in target.iterdir():
        assert path.read_bytes() == (source / path.name).read_bytes()
        assert path.stat().st_mode & 0o777 == 0o644


def test_real_app_staging_counts_nested_apps_and_installs_shared_parser(tmp_path):
    project = tmp_path / "project"
    for relative in ("alpha", "desktop-app", "gateway/slack"):
        app = project / "apps" / relative
        app.mkdir(parents=True)
        (app / "app.json").write_text(json.dumps({"id": relative.replace("/", "-")}))
    cached = project / "apps/gateway/email/__pycache__"
    cached.mkdir(parents=True)
    (cached / "main.pyc").write_bytes(b"stale migrated bytecode")
    shared = project / "apps/gateway/_shared"
    shared.mkdir()
    (shared / "__init__.py").write_text("SHARED_GATEWAY = True\n")
    (project / "apps/canonical_argv.py").write_text("SHARED_PARSER = True\n")
    packaging = project / "packaging/deb"
    (packaging / "claw-os-desktop").mkdir(parents=True)
    (packaging / "claw-os-desktop/apps.list").write_text("desktop-app\n")
    (project / "scripts").mkdir()
    (project / "scripts/app_sources.py").write_text(
        'from pathlib import Path\n'
        'import sys\n'
        'if "--stage" in sys.argv:\n'
        '    app = Path(sys.argv[sys.argv.index("--stage") + 1]) / "usr/lib/cos/apps/gateway/email"\n'
        '    app.mkdir(parents=True)\n'
        '    (app / "app.json").write_text(\'{"id":"gateway-email"}\')\n'
        'print(1)\n'
    )
    stage = tmp_path / "stage"
    (stage / "usr/lib/cos/apps").mkdir(parents=True)
    (stage / "usr/lib/cos/python").mkdir()
    script = (ROOT / "packaging/deb/build-debs.sh").read_text()
    block = script.split("# All non-graphical apps", 1)[1]
    block = block[block.index("DESKTOP_APPS_FILE="):]
    block = block.split('if [ -d "$PROJECT_DIR/skills" ]; then', 1)[0]
    subprocess.run(["bash", "-euc", block], check=True, env={
        **os.environ, "PROJECT_DIR": str(project), "SCRIPT_DIR": str(packaging),
        "AGENT_STAGE": str(stage),
    })
    assert (stage / "usr/lib/cos/apps/gateway/email/app.json").is_file()
    assert (stage / "usr/lib/cos/apps/gateway/slack/app.json").is_file()
    assert not (stage / "usr/lib/cos/apps/desktop-app").exists()
    assert not (stage / "usr/lib/cos/apps/gateway/email/__pycache__").exists()
    assert (stage / "usr/lib/cos/apps/gateway/_shared/__init__.py").is_file()
    assert (cached / "main.pyc").is_file()
    assert (stage / "usr/lib/cos/python/canonical_argv.py").read_text() == "SHARED_PARSER = True\n"


def _app_payload(directory, *, source=False):
    entries = {}
    for path in [directory, *sorted(directory.rglob("*"))]:
        relative = path.relative_to(directory)
        if source and any(
            part in {"__pycache__", ".pytest_cache"} or fnmatch.fnmatch(part, "test_*.py")
            for part in relative.parts
        ):
            continue
        if path.is_symlink():
            value = ("symlink", os.readlink(path))
        elif path.is_file():
            value = ("file", path.read_bytes())
        elif path.is_dir():
            value = ("directory",)
        else:
            raise AssertionError(f"Unsupported App payload entry: {path}")
        entries[relative] = (path.lstat().st_mode, value)
    return entries


def test_published_capability_pin_stages_all_partitions_and_real_agent_runtime(tmp_path):
    lock = sources.read_lock()
    assert lock["capabilities"] == ["document-engine", "storage-sdk", "http"]
    assert len(lock["products"]) == 24
    assert len(lock["apps"]) == 74
    source = sources.prepare_sources(lock)
    expected = set(lock["apps"])
    desktop = expected & set(sources.desktop_apps())
    assert len(desktop) == 12
    assert len(expected - desktop) == 62
    assert set(sources.stage_products(tmp_path / "all")) == expected
    assert set(sources.stage_products(tmp_path / "agent", "agent")) == expected - desktop
    assert set(sources.stage_products(tmp_path / "desktop", "desktop")) == desktop
    assert not (tmp_path / "desktop/usr/lib/cos/python/claw_files").exists()
    assert sources.app_path("doc") == source / "capabilities/document-engine/apps/doc"
    assert sources.app_path("db") == source / "capabilities/storage-sdk/apps/db"
    assert sources.app_path("kv") == source / "capabilities/storage-sdk/apps/kv"
    assert sources.app_path("net") == source / "capabilities/http/apps/net"
    assert not (ROOT / "apps/doc").exists()
    assert not (ROOT / "apps/db").exists()
    assert not (ROOT / "apps/kv").exists()
    assert not (ROOT / "apps/net").exists()
    for app_id in ("db", "kv", "net"):
        assert app_id not in desktop
        assert not (tmp_path / "desktop/usr/lib/cos/apps" / app_id).exists()

    staged = tmp_path / "agent-package"
    python = staged / "usr/lib/cos/python"
    python.mkdir(parents=True)
    (staged / "usr/lib/cos/apps").mkdir()
    for library in ("claw-os-sdk/python/src/claw_os_sdk", "cos-runtime/python/src/cos_runtime"):
        shutil.copytree(ROOT / library, python / Path(library).name,
                        ignore=shutil.ignore_patterns("__pycache__", "test_*.py"))
    script = (ROOT / "packaging/deb/build-debs.sh").read_text()
    block = script.split("# All non-graphical apps", 1)[1]
    block = block[block.index("DESKTOP_APPS_FILE="):]
    block = block.split('if [ -d "$PROJECT_DIR/skills" ]; then', 1)[0]
    subprocess.run(["bash", "-euc", block], check=True, env={
        **os.environ, "PROJECT_DIR": str(ROOT), "SCRIPT_DIR": str(ROOT / "packaging/deb"),
        "AGENT_STAGE": str(staged),
    })
    for app_id, group in (
        ("doc", "document-engine"), ("db", "storage-sdk"), ("kv", "storage-sdk"), ("net", "http"),
    ):
        original = source / "capabilities" / group / "apps" / app_id
        for partition_name in ("all", "agent", "agent-package"):
            installed = tmp_path / partition_name / "usr/lib/cos/apps" / app_id
            if app_id in {"db", "kv", "net"}:
                assert _app_payload(installed) == _app_payload(original, source=True)
            else:
                assert {path.name for path in installed.iterdir()} == {"app.json", "main.py", "server.py"}
                for name in ("app.json", "main.py", "server.py"):
                    assert (installed / name).read_bytes() == (original / name).read_bytes()
                    assert (installed / name).stat().st_mode == (original / name).stat().st_mode
    parser = python / "claw_files/document.py"
    assert parser.read_bytes() == (source / "products/files/python/claw_files/document.py").read_bytes()
    manifests = list((staged / "usr/lib/cos/apps").rglob("app.json"))
    assert len(manifests) == 63
    assert {json.loads(path.read_text())["id"] for path in manifests} == (
        expected - desktop
    ) | {"summarize"}
    document = tmp_path / "synthetic.txt"
    document.write_text("immutable capability in the real Agent composition")
    policy = tmp_path / "cos"
    policy.write_text(
        '#!/bin/sh\n'
        'test "$1:$2:$3" = "--wire=1:__policy:check" || exit 99\n'
        'printf \'%s\\n\' \'{"ok":true,"wire_version":1,"data":{"decision":"allow"}}\'\n'
    )
    policy.chmod(0o755)
    result = subprocess.run(
        [sys.executable, "-c",
         "import json; import main; import claw_files.document as library; "
         "print(json.dumps({'library':library.__file__, 'result':main.run('read', "
         f"[{str(document)!r}])}}))"],
        cwd=staged / "usr/lib/cos/apps/doc", capture_output=True, text=True, check=True, timeout=20,
        env={"PATH": os.defpath, "PYTHONPATH": str(python),
             "PYTHONDONTWRITEBYTECODE": "1", "CLAW_COS_BIN": str(policy)},
    )
    payload = json.loads(result.stdout)
    assert payload["library"] == str(parser)
    assert payload["result"]["content"] == document.read_text()

    net = staged / "usr/lib/cos/apps/net"
    net_manifest = json.loads((net / "app.json").read_text())
    (net / net_manifest["mcp"]["entry"]).rename(net / "http_mcp.py")
    net_manifest["mcp"]["entry"] = "http_mcp.py"
    (net / "app.json").write_text(json.dumps(net_manifest))
    denied_policy = tmp_path / "deny-cos"
    denied_policy.write_text(
        '#!/bin/sh\n'
        'test "$1:$2:$3" = "--wire=1:__policy:check" || exit 99\n'
        'printf \'%s\\n\' \'{"ok":true,"wire_version":1,"data":{"decision":"deny"}}\'\n'
    )
    denied_policy.chmod(0o755)
    output = tmp_path / "not-downloaded.bin"
    with mcp_process(net, env={
        "PATH": os.defpath,
        "PYTHONPATH": os.pathsep.join([str(python), str(staged / "usr/lib/cos/apps")]),
        "COS_DATA_DIR": str(tmp_path / "net-data"), "CLAW_COS_BIN": str(denied_policy),
    }) as request:
        catalog = request("tools/list", {})
        assert {tool["name"] for tool in catalog["tools"]} == {"net.fetch", "net.download"}
        for name, arguments in [
            ("fetch", {"url": "http://example.test:8080/resource"}),
            ("download", {"url": "http://example.test:8080/resource", "output": str(output)}),
        ]:
            result = request("tools/call", authenticated_mcp_params({
                "name": f"net.{name}", "arguments": arguments,
            }))
            assert result["isError"] is True
            assert "PermissionDenied" in result["content"][0]["text"]
    assert not output.exists()

    data = tmp_path / "owner-data/apps/db"
    database = data / "db/inventory.db"
    database.parent.mkdir(parents=True)
    with sqlite3.connect(database) as connection:
        connection.execute("CREATE TABLE items (value TEXT)")
        connection.execute("INSERT INTO items VALUES ('existing state')")
    identity = (database.stat().st_dev, database.stat().st_ino)
    neighbours = [
        tmp_path / "owner-data/apps/kv/kv.json",
        tmp_path / "owner-data/agent/memory.db",
        tmp_path / "other-owner/apps/db/db/inventory.db",
    ]
    for neighbour in neighbours:
        neighbour.parent.mkdir(parents=True, exist_ok=True)
        neighbour.write_bytes(b"unrelated private state")
    db_policy = tmp_path / "db-policy"
    db_policy.write_text(
        '#!/bin/sh\n'
        'test "$1:$2:$3" = "--wire=1:__policy:check" || exit 99\n'
        'test "$COS_APP_ID:$COS_SESSION" = "db:db-fixture" || exit 98\n'
        'case "$4:$5:$6" in\n'
        '  data.db.read:--name:inventory|data.db.write:--name:inventory) decision=allow;;\n'
        '  *) decision=deny;;\n'
        'esac\n'
        'printf \'{"ok":true,"wire_version":1,"data":{"decision":"%s"}}\\n\' "$decision"\n'
    )
    db_policy.chmod(0o755)
    calls = [
        ("query", {"database": "inventory", "sql": "SELECT value FROM items"}),
        ("exec", {"database": "inventory", "sql": "UPDATE items SET value = 'updated state'"}),
        ("query", {"database": "inventory", "sql": "SELECT value FROM items"}),
        ("query", {
            "database": "inventory",
            "sql": "WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM seq WHERE n<1001) SELECT n FROM seq",
        }),
        ("schema", {"database": "inventory", "table": "items"}),
        ("tables", {"database": "inventory"}),
        ("exec", {"database": "other", "sql": "CREATE TABLE denied (value)"}),
        ("databases", {}),
        ("query", {"database": "inventory", "sql": "DELETE FROM items"}),
    ]
    db_app = staged / "usr/lib/cos/apps/db"
    with mcp_process(db_app, env={
        "PATH": os.defpath, "PYTHONPATH": str(python), "COS_DATA_DIR": str(data),
        "COS_SESSION": "db-fixture", "CLAW_COS_BIN": str(db_policy),
    }) as request:
        results = [
            request("tools/call", authenticated_mcp_params({
                "name": f"db.{command}", "arguments": arguments,
            }))
            for command, arguments in calls
        ]
    assert results[0]["structuredContent"] == {
        "database": "inventory", "columns": ["value"], "rows": [["existing state"]], "count": 1,
    }
    assert results[1]["structuredContent"]["rows_affected"] == 1
    assert results[2]["structuredContent"]["rows"] == [["updated state"]]
    assert results[3]["structuredContent"] == {
        "database": "inventory", "columns": ["n"], "rows": [[i] for i in range(1, 1001)],
        "count": 1000, "truncated": True, "total_rows": 1001,
    }
    assert results[4]["structuredContent"] == {
        "database": "inventory", "table": "items", "schema": "CREATE TABLE items (value TEXT)",
    }
    assert results[5]["structuredContent"] == {"database": "inventory", "tables": ["items"]}
    for denied in results[6:8]:
        assert denied["isError"] is True
        assert "PermissionDenied" in denied["content"][0]["text"]
    assert results[8]["isError"] is True
    assert "database query failed" in results[8]["content"][0]["text"]
    with sqlite3.connect(database) as connection:
        assert connection.execute("SELECT value FROM items").fetchall() == [("updated state",)]
    assert (database.stat().st_dev, database.stat().st_ino) == identity
    assert [path.name for path in database.parent.iterdir()] == ["inventory.db"]
    assert all(neighbour.read_bytes() == b"unrelated private state" for neighbour in neighbours)
    assert not (data.parent / "storage-sdk").exists()

    kv_data = tmp_path / "kv-owner-data/apps/kv"
    kv_data.mkdir(parents=True)
    store = kv_data / "kv.json"
    original = b'{\n  "kept": "existing state", "empty": ""\n}\n'
    store.write_bytes(original)
    kv_neighbours = [
        tmp_path / "kv-owner-data/apps/db/db/existing.db",
        tmp_path / "kv-owner-data/apps/storage-manager/state",
        tmp_path / "kv-owner-data/agent/memory.db",
        tmp_path / "kv-other-owner/apps/kv/kv.json",
    ]
    for neighbour in kv_neighbours:
        neighbour.parent.mkdir(parents=True, exist_ok=True)
        neighbour.write_bytes(b"unrelated private state")
    kv_app = staged / "usr/lib/cos/apps/kv"
    environment = {
        "PATH": os.defpath, "PYTHONPATH": os.pathsep.join([str(python), str(kv_app.parent)]),
        "COS_DATA_DIR": str(kv_data), "COS_SESSION": "kv-fixture",
    }

    def kv_call(request, command, arguments=None):
        return request("tools/call", authenticated_mcp_params({
            "name": f"kv.{command}", "arguments": arguments or {},
        }))

    with (
        store.open("rb") as old,
        mcp_process(kv_app, env=environment) as first,
        mcp_process(kv_app, env=environment) as second,
    ):
        identity = os.fstat(old.fileno())
        catalog = first("tools/list", {})["tools"]
        assert [tool["name"] for tool in catalog] == [
            "kv.get", "kv.set", "kv.del", "kv.list", "kv.dump",
        ]
        assert catalog[3]["inputSchema"]["properties"]["pattern"]["default"] == "*"
        assert kv_call(first, "get", {"key": "kept"})["content"][0]["text"] == "existing state"
        assert kv_call(second, "dump")["structuredContent"] == {
            "count": 2, "data": {"kept": "existing state", "empty": ""},
        }
        assert store.read_bytes() == original
        for peer, key, value in ((first, "left", "one"), (second, "right", "two")):
            assert kv_call(peer, "set", {"key": key, "value": value})["structuredContent"] == {
                "key": key, "value": value,
            }
        assert kv_call(first, "list")["structuredContent"] == {
            "pattern": "*", "keys": ["empty", "kept", "left", "right"],
        }
        assert kv_call(second, "list", {"pattern": "l*"})["structuredContent"] == {
            "pattern": "l*", "keys": ["left"],
        }
        assert kv_call(first, "del", {"key": "left"})["structuredContent"] == {
            "key": "left", "deleted": True,
        }
        assert kv_call(second, "get", {"key": "left"})["content"][0]["text"] == ""
        assert old.read() == original
        assert (store.stat().st_dev, store.stat().st_ino) != (identity.st_dev, identity.st_ino)
        for command, arguments in [
            ("get", {"key": 42}), ("set", {"key": "bad", "value": False}),
            ("dump", {"session_id": "forged"}),
        ]:
            assert kv_call(first, command, arguments)["isError"] is True
    expected = {"kept": "existing state", "empty": "", "right": "two"}
    assert json.loads(store.read_text()) == expected
    assert store.stat().st_mode & 0o777 == 0o600
    assert (kv_data / "kv.json.lock").stat().st_mode & 0o777 == 0o600
    assert {path.name for path in kv_data.iterdir()} == {"kv.json", "kv.json.lock"}
    with mcp_process(kv_app, env=environment) as restarted:
        assert kv_call(restarted, "dump")["structuredContent"] == {"count": 3, "data": expected}
        store.write_bytes(b'{"corrupt": false}')
        for command, arguments in [("dump", {}), ("set", {"key": "new", "value": "value"})]:
            result = kv_call(restarted, command, arguments)
            assert result["isError"] is True
            assert "kv store must contain only string keys and values" in result["content"][0]["text"]
            assert store.read_bytes() == b'{"corrupt": false}'
    assert all(neighbour.read_bytes() == b"unrelated private state" for neighbour in kv_neighbours)
    assert not (kv_data.parent / "storage-sdk").exists()
    assert not (tmp_path / "kv-owner-data/kv.json").exists()


def test_native_inputs_follow_lock_updates_without_stale_files(locked_source):
    root, lock = locked_source
    upstream = Path(lock["repository"])
    (upstream / "tools/stage_native.py").write_text(
        'import json, pathlib, shutil, sys\n'
        'root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])\n'
        'product = sys.argv[1]\n'
        'shutil.copytree(pathlib.Path(__file__).parents[1] / "products" / product / "native", root / ("claw-applet-" + product))\n'
        'print(json.dumps(["claw-applet-" + product]))\n'
    )
    lock["products"] = ["calendar", "clipboard"]
    for product in lock["products"]:
        product_root = upstream / "products" / product
        (product_root / "native").mkdir(parents=True)
        (product_root / "native/Cargo.toml").write_text(product)
        (product_root / "package.json").write_text(json.dumps({
            "native": {f"claw-applet-{product}": "native"},
        }))
    native = upstream / "products/calendar/native"
    (native / "Cargo.toml").write_text("first")
    (native / "removed.rs").write_text("old")
    git = ["git", "-C", str(upstream)]

    def publish():
        subprocess.run([*git, "add", "tools/stage_native.py", "products"], check=True)
        subprocess.run([*git, "commit", "--quiet", "-m", "native fixture"], check=True)
        lock["revision"] = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
        (root / "packaging/apps.lock.json").write_text(json.dumps(lock))

    publish()
    destination = sources.prepare_native()
    assert (destination / "claw-applet-clipboard/Cargo.toml").read_text() == "clipboard"
    (destination / "unrelated").mkdir()
    (destination / "unrelated/keep").write_text("preserve")
    assert (destination / "claw-applet-calendar/removed.rs").is_file()
    (native / "removed.rs").unlink()
    (native / "Cargo.toml").write_text("second")
    publish()
    assert sources.prepare_native() == destination
    assert (destination / "claw-applet-calendar/Cargo.toml").read_text() == "second"
    assert not (destination / "claw-applet-calendar/removed.rs").exists()
    assert (destination / "unrelated/keep").read_text() == "preserve"
    assert (destination / "revision").read_text().strip() == lock["revision"]
    cached = sources.prepare_sources(lock)
    (cached / "products/calendar/native/Cargo.toml").write_text("tampered")
    with pytest.raises(RuntimeError, match="modified"):
        sources.prepare_native()


def test_native_manual_image_and_asset_build_paths_agree():
    applets = ROOT / "desktop/applets"
    cargo = (applets / "cosmic-applets/Cargo.toml").read_text()
    assert '../../../build/native-apps/claw-applet-calendar' in cargo
    assert '../../../build/native-apps/claw-applet-clipboard' in cargo
    assert '../../../build/native-apps/claw-applet-widget-rail' in cargo
    just = (applets / "justfile").read_text()
    assert 'build-debug *args: prepare-apps' in just
    assert 'python3 ../../scripts/app_sources.py --native' in just
    assert "(_install_icons calendar-src)" in just
    assert "_install_calendar" in just.split("install:", 1)[1]
    assert "_install_clipboard" in just.split("install:", 1)[1]
    assert "_install_widget_rail" in just.split("install:", 1)[1]
    assert "test -f {{ widget-rail-src }}/Cargo.toml" in just
    assert "test -f {{ clipboard-src }}/Cargo.toml" in just
    script = (ROOT / "rootfs/features/desktop/install.sh").read_text()
    assert 'CHROOT_NATIVE_APPS="$ROOTFS/build/build/native-apps"' in script
    assert 'mount --bind "$NATIVE_APP_SOURCES" "$CHROOT_NATIVE_APPS"' in script
    assert 'CLAW_NATIVE_APPS_PREPARED=1' in script
    assert 'umount "$CHROOT_NATIVE_APPS"' in script
    assert '../../../build/native-apps/{name}/data/{id}.desktop' in (
        applets / "cosmic-applets/build.rs"
    ).read_text()


@pytest.mark.parametrize(("product", "app_id"), [
    ("calendar", "panel-calendar"), ("clipboard", "panel-clipboard"),
    ("desktop-widgets", "widget-rail"),
    ("launcher", "cosmic-launcher"),
    ("editor", "cosmic-edit"),
    ("files", "cosmic-files"),
    ("terminal", "cosmic-term"),
    ("store", "cosmic-store"),
    ("settings", "cosmic-settings"),
    ("capture", "cosmic-screenshot"),
    ("media-player", "cosmic-player"),
    ("notifications", "cosmic-notifications"),
])
def test_external_desktop_app_stays_out_of_agent_package(locked_source, tmp_path, product, app_id):
    root, lock = locked_source
    upstream = Path(lock["repository"])
    package = upstream / "products" / product
    app = package / "apps" / app_id
    app.mkdir(parents=True)
    (app / "app.json").write_text(json.dumps({"id": app_id}))
    (package / "package.json").write_text(json.dumps({"apps": [f"apps/{app_id}"]}))
    (upstream / "tools/stage.py").write_text(
        'import json, pathlib, shutil, sys\n'
        'root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])\n'
        'ids = sys.argv[sys.argv.index("--apps") + 1:]\n'
        f'if "{app_id}" in ids:\n'
        f'    source = pathlib.Path(__file__).parents[1] / "products/{product}/apps/{app_id}"\n'
        f'    shutil.copytree(source, root / "usr/lib/cos/apps/{app_id}")\n'
        'print(json.dumps(ids))\n'
    )
    git = ["git", "-C", str(upstream)]
    subprocess.run([*git, "add", f"products/{product}", "tools/stage.py"], check=True)
    subprocess.run([*git, "commit", "--quiet", "-m", "desktop fixture"], check=True)
    lock["revision"] = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    lock["products"] = [product]
    lock["apps"] = [app_id]
    (root / "packaging/apps.lock.json").write_text(json.dumps(lock))
    partition = root / "packaging/deb/claw-os-desktop/apps.list"
    partition.parent.mkdir(parents=True)
    partition.write_text(app_id + "\n")
    assert sources.stage_products(tmp_path / "agent", "agent") == []
    assert not (tmp_path / "agent/usr/lib/cos/apps" / app_id).exists()
    assert sources.stage_products(tmp_path / "desktop", "desktop") == [app_id]
    assert (tmp_path / "desktop/usr/lib/cos/apps" / app_id / "app.json").is_file()
    assert sources.app_path(app_id).name == app_id
    with pytest.raises(ValueError, match="not in the source lock"):
        sources.app_path("unknown")
    (root / "scripts").mkdir()
    shutil.copyfile(ROOT / "scripts/app_sources.py", root / "scripts/app_sources.py")
    script = (ROOT / "packaging/deb/build-desktop-deb.sh").read_text()
    block = script[script.index("DESKTOP_APPS_FILE="):script.index("THERMALD_DEP=")]
    stage = tmp_path / "desktop-deb"
    subprocess.run(["bash", "-euc", block], check=True, env={
        **os.environ, "PROJECT_DIR": str(root), "SCRIPT_DIR": str(root / "packaging/deb"),
        "STAGE_ROOT": str(stage),
    })
    assert (stage / "usr/lib/cos/apps" / app_id / "app.json").is_file()


@pytest.mark.parametrize("components", [
    {"../escape": "native"},
    {"valid": "../escape"},
    {"valid": 123},
    ["not-a-map"],
])
def test_invalid_native_exports_are_rejected_before_replacement(tmp_path, monkeypatch, components):
    source = tmp_path / "source"
    product = source / "products/clipboard"
    product.mkdir(parents=True)
    (product / "package.json").write_text(json.dumps({"native": components}))
    monkeypatch.setattr(sources, "ROOT", tmp_path)
    monkeypatch.setattr(sources, "read_lock", lambda: {"products": ["clipboard"]})
    monkeypatch.setattr(sources, "prepare_sources", lambda _: source)
    with pytest.raises(ValueError, match="native|Native"):
        sources.prepare_native()
    assert not (tmp_path / "build/native-apps").exists()


def test_duplicate_native_exports_are_rejected(tmp_path, monkeypatch):
    for product in ("calendar", "clipboard"):
        root = tmp_path / "products" / product
        root.mkdir(parents=True)
        (root / "package.json").write_text('{"native":{"duplicate":"native"}}')
    monkeypatch.setattr(sources, "read_lock", lambda: {"products": ["calendar", "clipboard"]})
    monkeypatch.setattr(sources, "prepare_sources", lambda _: tmp_path)
    with pytest.raises(ValueError, match="duplicate"):
        sources.prepare_native()


@pytest.mark.parametrize(("product", "component"), [
    ("launcher", "launcher"), ("editor", "edit"), ("files", "files"), ("terminal", "term"),
    ("store", "store"), ("settings", "settings"), ("capture", "screenshot"), ("player", "player"),
    ("notifications", "notifications"),
])
def test_standalone_app_uses_external_source_and_matching_chroot_layout(product, component):
    just = (ROOT / "desktop/justfile").read_text()
    assert f"{product} := '../build/native-apps/cosmic-{component}/justfile'" in just
    assert f"{product}-build: prepare-native-apps" in just
    assert f"test -f {{{{ {product} }}}}" in just
    assert "[default]\nbuild:" in just
    assert "build: prepare-native-apps\n    #!/usr/bin/env bash" in just
    assert "export CLAW_NATIVE_APPS_PREPARED=1" in just
    assert "{{ just }} --justfile {{ " + product + " }} build-release --locked" in just
    assert "{{ just }} --justfile {{ " + product + " }} rootdir={{rootdir}} prefix={{prefix}} install" in just
    assert "{{ just }} " + component + "/build-release" not in just
    script = (ROOT / "rootfs/features/desktop/install.sh").read_text()
    assert 'CHROOT_SRC="$ROOTFS/build/desktop"' in script
    assert "cd /build/desktop" in script
    assert "/build/desktop-src" not in script
    assert not (ROOT / "desktop" / component).exists()
    assert not (ROOT / "apps" / f"cosmic-{component}").exists()
    assert f"cosmic-{component}" in sources.read_lock()["apps"]
    assert f"cosmic-{component}" in sources.desktop_apps()


@pytest.mark.skipif(shutil.which("just") is None, reason="desktop just runner unavailable")
def test_desktop_build_prepares_once_and_propagates_prepared_inputs(tmp_path):
    desktop = tmp_path / "desktop"
    desktop.mkdir()
    shutil.copyfile(ROOT / "desktop/justfile", desktop / "justfile")
    scripts = tmp_path / "scripts"
    scripts.mkdir()
    (scripts / "app_sources.py").write_text(
        "from pathlib import Path\n"
        "root = Path(__file__).resolve().parents[1]\n"
        "with (root / 'prepared').open('a') as log: log.write('prepared\\n')\n"
    )
    runner = tmp_path / "runner"
    runner.write_text(
        "#!/bin/sh\n"
        'test "$CLAW_NATIVE_APPS_PREPARED" = 1 || exit 97\n'
        'printf "%s\\n" "$*" >> "$0.calls"\n'
    )
    runner.chmod(0o755)
    env = os.environ.copy()
    env.pop("CLAW_NATIVE_APPS_PREPARED", None)
    subprocess.run(
        ["just", "--justfile", str(desktop / "justfile"),
         f"just={runner}", f"make={runner}", "build"],
        check=True, env=env, capture_output=True, text=True,
    )
    assert (tmp_path / "prepared").read_text() == "prepared\n"
    calls = (tmp_path / "runner.calls").read_text().splitlines()
    assert calls.count("editor-build") == 1
    assert calls.count("files-build") == 1
    assert calls.count("launcher-build") == 1
    assert calls.count("terminal-build") == 1
    assert calls.count("store-build") == 1
    assert calls.count("settings-build") == 1
    assert calls.count("capture-build") == 1
    assert calls.count("player-build") == 1
    assert calls.count("notifications-build") == 1
    assert calls.count("applets/build-release") == 1


def test_media_player_is_package_only_for_hot_swap(tmp_path):
    result = subprocess.run(
        ["bash", str(ROOT / "scripts/hot-swap-to-vmware.sh"), "--no-build", "cosmic-player"],
        env={**os.environ, "QCOW": str(tmp_path / "must-not-be-opened.qcow2")},
        capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "Media Player requires its signed manifest and claw-os-media-player-v1" in result.stderr
    assert "qcow2 missing" not in result.stderr


def test_notifications_is_package_only_for_hot_swap(tmp_path):
    result = subprocess.run(
        ["bash", str(ROOT / "scripts/hot-swap-to-vmware.sh"), "--no-build", "cosmic-notifications"],
        env={**os.environ, "QCOW": str(tmp_path / "must-not-be-opened.qcow2")},
        capture_output=True, text=True,
    )
    assert result.returncode != 0
    assert "Notifications requires its signed manifest and claw-os-notifications-v1" in result.stderr
    assert "qcow2 missing" not in result.stderr


def test_native_notifications_share_exported_libraries_and_leave_legacy_state_separate():
    lock = sources.read_lock()
    assert lock["apps"].count("cosmic-notifications") == 1
    assert "notifications" in lock["products"]
    assert not (ROOT / "desktop/notifications").exists()
    assert not (ROOT / "apps/cosmic-notifications").exists()
    assert not (ROOT / "apps/notify").exists()
    assert lock["apps"].count("notify") == 1
    applet = (ROOT / "desktop/applets/cosmic-applet-notifications/Cargo.toml").read_text()
    panel = (ROOT / "desktop/panel/cosmic-panel-bin/Cargo.toml").read_text()
    for library in ("cosmic-notifications-config", "cosmic-notifications-util"):
        assert f'../../../build/native-apps/cosmic-notifications/{library}' in applet
        assert f'cosmic-notifications/{library}/Cargo.toml' in (
            ROOT / "rootfs/features/desktop/install.sh"
        ).read_text()
    assert "../../../build/native-apps/cosmic-notifications/cosmic-notifications-config" in panel
    assert 'git = "https://github.com/pop-os/cosmic-notifications"' not in applet + panel
    panel_just = (ROOT / "desktop/panel/justfile").read_text()
    assert "build-debug *args: prepare-apps" in panel_just
    assert "check *args: prepare-apps" in panel_just
    assert "python3 ../../scripts/app_sources.py --native" in panel_just
    status = (ROOT / "docs/app-product-redesign.md").read_text()
    assert "clawos-app/products/notifications" in status
    assert "**74 of the original 75 identities**" in status
    assert "**24 business product groups**" in status
    assert "62 Agent-package identities and 12 desktop identities" in status
    assert "products/notifications" in (ROOT / "desktop/PROVENANCE.md").read_text()
    assert "just notifications-build" in (ROOT / "desktop/README.md").read_text()


def test_notify_is_agent_owned_and_preserves_history_without_an_installer_transition():
    lock = sources.read_lock()
    desktop = set(sources.desktop_apps())
    assert len(lock["apps"]) == 74
    assert len(lock["products"]) == 24
    assert lock["capabilities"] == ["document-engine", "storage-sdk", "http"]
    assert len(set(lock["apps"]) & desktop) == 12
    assert len(set(lock["apps"]) - desktop) == 62
    assert "notify" not in desktop
    assert "cosmic-notifications" in desktop
    assert not (ROOT / "apps/notify").exists()
    migration = (ROOT / "core/src/worker/migrate.rs").read_text()
    assert '("notify", &[Legacy::File("notifications.json")])' not in migration
    contract = (ROOT / "docs/updating.md").read_text()
    assert "Historical JSON is preserved, not imported" in contract
    assert "not part of the new service list" in contract
    assert "data.inbox.read" in contract
    assert "warning severity" in contract
    assert "62 migrated Agent identities plus 12 desktop" in (ROOT / "packaging/MODULE.md").read_text()


@pytest.mark.parametrize("export", [
    {"component": "other", "path": "public"},
    {"component": "daemon", "path": "../outside"},
    {"component": "daemon", "path": "/private"},
    {"component": [], "path": "public"},
    {"component": "daemon", "path": "public", "authority": True},
])
def test_invalid_public_native_libraries_are_refused_before_replacement(tmp_path, monkeypatch, export):
    product = tmp_path / "source/products/notifications"
    product.mkdir(parents=True)
    (product / "package.json").write_text(json.dumps({
        "native": {"daemon": "native"}, "native_libraries": {"public-library": export},
    }))
    monkeypatch.setattr(sources, "ROOT", tmp_path)
    monkeypatch.setattr(sources, "read_lock", lambda: {"products": ["notifications"]})
    monkeypatch.setattr(sources, "prepare_sources", lambda _: tmp_path / "source")
    with pytest.raises(ValueError, match="native|Native"):
        sources.prepare_native()
    assert not (tmp_path / "build/native-apps").exists()


def test_native_library_paths_and_cargo_identity_are_bound_to_the_product(tmp_path):
    library = tmp_path / "native/public"
    library.mkdir(parents=True)
    (library / "Cargo.toml").write_text('[package]\nname = "public-library"\n')
    package = {
        "native": {"daemon": "native"},
        "native_libraries": {"public-library": {"component": "daemon", "path": "public"}},
    }
    assert sources.native_libraries(package, tmp_path) == {"public-library": "daemon/public"}
    (library / "Cargo.toml").write_text('[package]\nname = "another-library"\n')
    with pytest.raises(ValueError, match="identity"):
        sources.native_libraries(package, tmp_path)


def test_native_player_source_status_and_package_identity_are_consistent():
    lock = sources.read_lock()
    assert "media-player" in lock["products"]
    assert lock["apps"].count("cosmic-player") == 1
    assert "cosmic-player" in sources.desktop_apps()
    assert not (ROOT / "desktop/player").exists()
    assert not (ROOT / "apps/cosmic-player").exists()
    status = (ROOT / "docs/app-product-redesign.md").read_text()
    assert f"**{len(lock['apps'])} of the original 75 identities**" in status
    desktop = len(set(lock["apps"]) & set(sources.desktop_apps()))
    assert f"{len(lock['apps']) - desktop} Agent-package identities and {desktop} desktop identities" in status
    assert f"**{len(lock['products'])} business product groups**" in status
    assert "clawos-app/products/media-player" in status
    assert "products/media-player" in (ROOT / "desktop/PROVENANCE.md").read_text()
    assert "just player-build" in (ROOT / "desktop/README.md").read_text()
