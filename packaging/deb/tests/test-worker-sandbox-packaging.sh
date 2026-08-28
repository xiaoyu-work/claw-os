#!/bin/bash
# packaging/deb/tests/test-worker-sandbox-packaging.sh -- the installed
# system must be able to isolate hostile workers.
#
# Every App operation, GUI surface, MCP server and adapter runs inside a
# bubblewrap sandbox, and the launch fails closed when bubblewrap is
# missing or too old to disable nested user namespaces. That makes the
# dependency a functional requirement of the package, not a
# recommendation: without it the agent installs successfully and then
# refuses to run any App. It is contract-checked here so the constraint
# cannot drift away from what the launcher demands at runtime.

set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)

CONTROL="$PROJECT_DIR/packaging/deb/claw-os-agent/control"
PROVIDER="$PROJECT_DIR/core/src/worker/linux.rs"

fail() {
    printf 'not ok - %s\n' "$*" >&2
    exit 1
}

assert_contains() {
    file=$1
    pattern=$2
    reason=$3
    grep -Fq -- "$pattern" "$file" || fail "$reason ($file is missing '$pattern')"
}

# The launcher refuses to start a worker without `--disable-userns`,
# which bubblewrap grew in 0.8. A package that depends on an older one
# would install an agent that cannot run an App.
assert_contains "$CONTROL" 'bubblewrap (>= 0.8.0)' \
    "claw-os-agent must depend on a bubblewrap that can disable nested user namespaces"

# The flags the dependency exists for.
for flag in --disable-userns --assert-userns-disabled --unshare-pid --unshare-net --seccomp; do
    assert_contains "$PROVIDER" "\"$flag\"" \
        "the Linux worker provider must pass $flag"
done

# A sandbox that cannot be built must not fall back to a bare process.
assert_contains "$PROVIDER" 'worker isolation unavailable' \
    "the provider must fail closed when bubblewrap is missing"

# Nothing may be fetched while a worker is being launched: the sandbox
# has no network, and the launcher must not paper over that by
# installing something first.
if grep -Eq '(apt-get|pip install|npm install|curl -[^ ]*O)' "$PROVIDER"; then
    fail "the worker provider must not install or download anything at launch time"
fi

printf 'ok - worker sandbox packaging contract\n'
