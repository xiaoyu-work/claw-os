#!/bin/bash
# packaging/deb/tests/test-agentd-packaging.sh -- the unprivileged agent
# worker and extension host must be installed wherever clawd is.
#
# clawd no longer runs the model/tool loop in its own process: it spawns
# /usr/local/bin/claw-agentd per task. An install that ships the broker
# without the worker, or a unit that never points at it, would leave
# every agent task failing, so both are contract-checked here.

set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)

BUILD_DEBS="$PROJECT_DIR/packaging/deb/build-debs.sh"
UNIT="$PROJECT_DIR/rootfs/features/systemd/overlay/usr/lib/systemd/system/clawd.service"
CARGO_TOML="$PROJECT_DIR/core/Cargo.toml"
SYSUSERS="$PROJECT_DIR/packaging/deb/claw-os-agent/claw-os-agent.sysusers"
POSTINST="$PROJECT_DIR/packaging/deb/claw-os-agent/postinst"

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

bash -n "$BUILD_DEBS" || fail "build-debs.sh is not valid bash"

assert_contains "$CARGO_TOML" 'name = "claw-agentd"' \
    "the agent worker must be a first-class cargo binary"
assert_contains "$CARGO_TOML" 'path = "src/bin/claw-agentd.rs"' \
    "the agent worker binary must have an entry point"
[ -f "$PROJECT_DIR/core/src/bin/claw-agentd.rs" ] ||
    fail "core/src/bin/claw-agentd.rs is missing"
assert_contains "$CARGO_TOML" 'name = "claw-extension-host"' \
    "the extension host must be a first-class cargo binary"
assert_contains "$CARGO_TOML" 'path = "src/bin/claw-extension-host.rs"' \
    "the extension host binary must have an entry point"
[ -f "$PROJECT_DIR/core/src/bin/claw-extension-host.rs" ] ||
    fail "core/src/bin/claw-extension-host.rs is missing"

assert_contains "$BUILD_DEBS" 'ensure_bin claw-agentd cos' \
    "claw-os-agent must build the agent worker"
assert_contains "$BUILD_DEBS" '/usr/local/bin/claw-agentd' \
    "claw-os-agent must install the agent worker beside clawd"
assert_contains "$BUILD_DEBS" 'ensure_bin claw-extension-host cos' \
    "claw-os-agent must build the extension host"
assert_contains "$BUILD_DEBS" '/usr/local/bin/claw-extension-host' \
    "claw-os-agent must install the extension host beside claw-agentd"
assert_contains "$BUILD_DEBS" '/usr/lib/sysusers.d/claw-os-agent.conf' \
    "claw-os-agent must install its dedicated extension group definition"
assert_contains "$SYSUSERS" 'g cos-extension - -' \
    "the extension host must have a dedicated system group"
assert_contains "$POSTINST" 'systemd-sysusers /usr/lib/sysusers.d/claw-os-agent.conf' \
    "postinst must create the extension execution group before starting clawd"

# The staged worker path and the path the unit hands clawd have to agree,
# or the daemon looks for a binary the package never installed.
assert_contains "$UNIT" 'COS_AGENTD_BIN=/usr/local/bin/claw-agentd' \
    "clawd.service must point at the installed agent worker"
assert_contains "$UNIT" 'COS_EXTENSION_HOST_BIN=/usr/local/bin/claw-extension-host' \
    "clawd.service must point at the installed extension host"
assert_contains "$UNIT" 'COS_EXTENSION_EXEC_GROUP=cos-extension' \
    "clawd.service must pin the dedicated extension execution group"
assert_contains "$UNIT" 'CLAWD_EXTENSION_HOST_NAMESPACES=on' \
    "clawd.service must enable available extension-host namespaces"
grep -Eq '^Delegate=yes$' "$UNIT" ||
    fail "clawd.service must delegate a cgroup subtree for extension cleanup"
assert_contains "$UNIT" 'CLAWD_AGENTD=on' \
    "clawd.service must state whether agent supervision is enabled"

# Traversable, never listable: an unprivileged worker needs to walk to
# /var/lib/cos/users/<uid>. Anything wider or narrower breaks the split.
grep -Eq '^StateDirectoryMode=0711$' "$UNIT" ||
    fail "clawd.service must set StateDirectoryMode=0711 for owner-partitioned agent state"

# The broker socket stays root:sudo 0660 and the worker never joins that
# group, so no route on it is reachable from a worker.
grep -Eq '^Environment=CLAWD_SOCKET_MODE=0660$' "$UNIT" ||
    fail "clawd.sock permissions must not be widened"
grep -Eq '^Group=sudo$' "$UNIT" ||
    fail "clawd.service must keep its socket group"

printf 'ok - agent isolation packaging contract\n'
