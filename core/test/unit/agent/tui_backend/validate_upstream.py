"""Validate adapter-generated fixtures against the pinned, local Codex schemas."""

import argparse
import json
from pathlib import Path
import subprocess
import sys

import jsonschema


PINNED_REVISION = "a592c38c16cdd7623dacc9168926ebccedfb67d3"
FIXTURE_MARKER = "CLAW_TUI_FIXTURE "


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream", type=Path, required=True)
    args = parser.parse_args()
    upstream = args.upstream.resolve(strict=True)
    revision = subprocess.check_output(
        ["git", "-C", str(upstream), "rev-parse", "HEAD"], text=True
    ).strip()
    if revision != PINNED_REVISION:
        parser.error(f"expected upstream revision {PINNED_REVISION}, got {revision}")
    schema_root = upstream / "codex-rs" / "app-server-protocol" / "schema" / "json"
    root = Path(__file__).resolve().parents[5]
    print("Generating adapter fixtures without making model requests...", flush=True)
    result = subprocess.run(
        [
            "cargo", "test", "-p", "cos", "--lib",
            "agent::tui_backend::tests::upstream_schema_fixtures",
            "--", "--exact", "--nocapture", "--test-threads=1",
        ],
        cwd=root,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode:
        sys.stdout.write(result.stdout)
        sys.stderr.write(result.stderr)
        return result.returncode
    validated = 0
    failures = []
    for line in result.stdout.splitlines():
        marker = line.find(FIXTURE_MARKER)
        if marker < 0:
            continue
        fixture = json.loads(line[marker + len(FIXTURE_MARKER):])
        relative = Path(fixture["schema"])
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("invalid schema fixture path")
        schema = json.loads((schema_root / relative).read_text(encoding="utf-8"))
        validator = jsonschema.validators.validator_for(schema)
        validator.check_schema(schema)
        errors = list(validator(schema).iter_errors(fixture["value"]))
        for error in errors:
            path = ".".join(str(part) for part in error.absolute_path) or "<root>"
            failures.append(f"{relative}: {path}: {error.message}")
        validated += 1
    if not validated:
        raise RuntimeError("the fixture test produced no protocol samples")
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(f"Validated {validated} adapter payloads against Codex {PINNED_REVISION}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
