"""Resolve pinned application sources for OS package assembly, never at runtime."""

import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]


def read_lock():
    lock = json.loads((ROOT / "packaging" / "apps.lock.json").read_text())
    if lock["version"] != 1 or not re.fullmatch(r"[0-9a-f]{40}", lock["revision"]):
        raise ValueError("App sources require a version-1 lock with a full Git revision")
    for field in ("products", "apps"):
        values = lock[field]
        if not isinstance(values, list) or not values or any(
            not isinstance(value, str) or not re.fullmatch(r"[a-z][a-z0-9-]*", value)
            for value in values
        ) or len(set(values)) != len(values):
            raise ValueError(f"Invalid or duplicate {field} in App source lock")
    return lock


def prepare_sources(lock):
    revision = lock["revision"]
    destination = ROOT / "build" / "app-sources" / revision
    if not destination.exists():
        destination.mkdir(parents=True)
        git = ["git", "--no-optional-locks", "-C", str(destination)]
        subprocess.run([*git, "init", "--quiet"], check=True)
        subprocess.run([*git, "remote", "add", "origin", lock["repository"]], check=True)
        subprocess.run([*git, "fetch", "--quiet", "--depth=1", "origin", revision], check=True)
        subprocess.run([*git, "checkout", "--quiet", "--detach", revision], check=True)
    git = ["git", "-C", str(destination)]
    actual = subprocess.check_output([*git, "rev-parse", "HEAD"], text=True).strip()
    if actual != revision:
        raise RuntimeError("App source cache does not match the locked revision")
    dirty = subprocess.check_output(
        [*git, "status", "--porcelain", "--untracked-files=normal"], text=True
    )
    if dirty:
        raise RuntimeError(f"App source cache is modified: {destination}")
    return destination


def desktop_apps():
    return (ROOT / "packaging/deb/claw-os-desktop/apps.list").read_text().split()


def stage_products(destination, package=None):
    lock = read_lock()
    source = prepare_sources(lock)
    installed = []
    selected = lock["apps"]
    if package:
        desktop = set(desktop_apps())
        selected = [app for app in selected if (app in desktop) == (package == "desktop")]
    for product in lock["products"]:
        selection = ["--apps", *selected] if package else []
        result = subprocess.check_output(
            [sys.executable, str(source / "tools" / "stage.py"),
             product, "--root", str(destination.resolve()), *selection],
            cwd=source, text=True,
        )
        installed.extend(json.loads(result))
    if sorted(installed) != sorted(selected):
        raise RuntimeError("Staged App identities do not match the OS source lock")
    return installed


def native_libraries(package, product_root):
    exports = package.get("native_libraries", {})
    if not isinstance(exports, dict):
        raise ValueError("Invalid native library declarations")
    result = {}
    for name, export in exports.items():
        if not re.fullmatch(r"[a-z][a-z0-9-]*", name) or not isinstance(export, dict) or set(export) != {"component", "path"}:
            raise ValueError("Invalid native library declaration")
        component, relative = export["component"], export["path"]
        if not isinstance(component, str) or component not in package.get("native", {}) or not isinstance(relative, str) or not re.fullmatch(
            r"[a-z][a-z0-9_-]*(/[a-z][a-z0-9_-]*)*", relative
        ):
            raise ValueError("Native library must belong to its declared component")
        root = (product_root / package["native"][component]).resolve()
        library = (root / relative).resolve()
        if not root.is_relative_to(product_root.resolve()) or not library.is_relative_to(root):
            raise ValueError("Native library must belong to its product component")
        manifest = tomllib.loads((library / "Cargo.toml").read_text())
        if manifest.get("package", {}).get("name") != name:
            raise ValueError("Native library identity does not match its manifest")
        result[name] = f"{component}/{relative}"
    return result


def prepare_native():
    lock = read_lock()
    source = prepare_sources(lock)
    destination = ROOT / "build/native-apps"
    products = {}
    names = set()
    libraries = {}
    for product in lock["products"]:
        package = json.loads((source / "products" / product / "package.json").read_text())
        components = package.get("native", {})
        if not isinstance(components, dict):
            raise ValueError("Invalid native component declarations")
        for name, relative in components.items():
            if not re.fullmatch(r"[a-z][a-z0-9-]*", name) or name in names:
                raise ValueError("Invalid or duplicate native component name")
            product_root = source / "products" / product
            if not isinstance(relative, str) or not (
                product_root / relative
            ).resolve().is_relative_to(product_root.resolve()):
                raise ValueError("Native source must belong to the product")
            names.add(name)
        exports = native_libraries(package, source / "products" / product)
        if libraries.keys() & exports.keys():
            raise ValueError("Duplicate native library export")
        libraries.update(exports)
        if components:
            products[product] = sorted(components)
    destination.mkdir(parents=True, exist_ok=True)
    for product, components in products.items():
        for name in components:
            component = destination / name
            if component.is_symlink():
                component.unlink()
            elif component.exists():
                shutil.rmtree(component)
        result = subprocess.check_output(
            [sys.executable, str(source / "tools/stage_native.py"), product,
             "--root", str(destination)], text=True, cwd=source,
        )
        if json.loads(result) != components:
            raise RuntimeError(f"Unexpected native build inputs for {product}")
    (destination / "revision").write_text(lock["revision"] + "\n")
    (destination / "native-libraries.json").write_text(json.dumps({
        "revision": lock["revision"], "libraries": libraries,
    }, indent=2) + "\n")
    return destination


def app_path(app_id):
    lock = read_lock()
    if app_id not in lock["apps"]:
        raise ValueError(f"App is not in the source lock: {app_id}")
    source = prepare_sources(lock)
    for product in lock["products"]:
        root = source / "products" / product
        package = json.loads((root / "package.json").read_text())
        for relative in package["apps"]:
            app = root / relative
            if json.loads((app / "app.json").read_text())["id"] == app_id:
                return app
    raise RuntimeError(f"Locked App source is missing: {app_id}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stage", type=Path)
    parser.add_argument("--package", choices=["agent", "desktop"])
    parser.add_argument("--native", action="store_true")
    parser.add_argument("--app-path")
    parser.add_argument("--count", action="store_true")
    args = parser.parse_args()
    if args.native:
        print(prepare_native())
    elif args.app_path:
        print(app_path(args.app_path))
    elif args.count:
        print(len(read_lock()["apps"]))
    elif args.stage:
        print(len(stage_products(args.stage, args.package)))
    else:
        print(prepare_sources(read_lock()))
