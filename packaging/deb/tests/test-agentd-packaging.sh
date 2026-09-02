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
TEST_WORKFLOW="$PROJECT_DIR/.github/workflows/test.yml"
CARGO_TOML="$PROJECT_DIR/core/Cargo.toml"
SYSUSERS="$PROJECT_DIR/packaging/deb/claw-os-agent/claw-os-agent.sysusers"
PREINST="$PROJECT_DIR/packaging/deb/claw-os-agent/preinst"
POSTINST="$PROJECT_DIR/packaging/deb/claw-os-agent/postinst"
POSTRM="$PROJECT_DIR/packaging/deb/claw-os-agent/postrm"
IDENTITY_HELPER="$PROJECT_DIR/packaging/deb/claw-os-agent/extension-identities.sh"
IDENTITY_RS="$PROJECT_DIR/core/src/extension_host/identity.rs"

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
for shell in bash dash; do
    "$shell" -n "$PREINST" || fail "claw-os-agent preinst is not valid $shell"
    "$shell" -n "$POSTINST" || fail "claw-os-agent postinst is not valid $shell"
    "$shell" -n "$POSTRM" || fail "claw-os-agent postrm is not valid $shell"
    "$shell" -n "$IDENTITY_HELPER" || fail "extension identity helper is not valid $shell"
    for script in "$PREINST" "$POSTINST" "$POSTRM"; do
        {
            cat "$IDENTITY_HELPER"
            tail -n +2 "$script"
        } | "$shell" -n ||
            fail "assembled $(basename "$script") is not valid $shell"
    done
done

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
assert_contains "$PROJECT_DIR/packaging/deb/claw-os-agent/control" 'Depends: acl,' \
    "the package must install getfacl for execution-gid collision scans"
assert_contains "$PROJECT_DIR/packaging/deb/claw-os-agent/control" 'findutils' \
    "the package must install find for per-mount ownership scans"

assert_contains "$BUILD_DEBS" 'ensure_bin claw-agentd cos' \
    "claw-os-agent must build the agent worker"
assert_contains "$BUILD_DEBS" '/usr/local/bin/claw-agentd' \
    "claw-os-agent must install the agent worker beside clawd"
assert_contains "$BUILD_DEBS" 'ensure_bin claw-extension-host cos' \
    "claw-os-agent must build the extension host"
assert_contains "$BUILD_DEBS" '/usr/local/bin/claw-extension-host' \
    "claw-os-agent must install the extension host beside claw-agentd"
assert_contains "$BUILD_DEBS" '/usr/lib/cos/extensions' \
    "claw-os-agent must create the authenticated Agent extension package root"
assert_contains "$BUILD_DEBS" '/usr/lib/sysusers.d/claw-os-agent.conf' \
    "claw-os-agent must install its dedicated extension group definition"
assert_contains "$SYSUSERS" 'g cos-extension - -' \
    "sysusers must preserve a safely retained legacy extension gid"
assert_contains "$POSTINST" 'systemd-sysusers /usr/lib/sysusers.d/claw-os-agent.conf' \
    "postinst must create the extension execution group before starting clawd"
assert_contains "$BUILD_DEBS" 'claw-os-agent/preinst' \
    "claw-os-agent must ship an identity-provisioning preinst"
assert_contains "$BUILD_DEBS" 'extension-identities.sh' \
    "maintainer scripts must embed the shared identity policy"
assert_contains "$POSTINST" 'identity_provision upgrade "$2"' \
    "postinst must provision identities after dependencies and helper are unpacked"
if grep -Fq 'identity_provision "$1"' "$PREINST"; then
    fail "preinst must not run dependency-backed identity scans before unpack"
fi
provision_line=$(grep -n 'identity_provision upgrade "$2"' "$POSTINST" | cut -d: -f1)
sysusers_line=$(grep -n 'systemd-sysusers /usr/lib/sysusers.d/claw-os-agent.conf' "$POSTINST" |
    cut -d: -f1)
[ "$provision_line" -lt "$sysusers_line" ] ||
    fail "postinst must reserve the exact gid before sysusers observes it"
assert_contains "$PREINST" 'deb-systemd-invoke stop clawd.service' \
    "upgrade must stop clawd before validating a legacy execution gid"
assert_contains "$POSTINST" 'identity_finalize' \
    "postinst must validate and write the runtime reservation manifest"
assert_contains "$POSTRM" 'identity_purge_owned' \
    "purge must remove only identities proven package-owned"
assert_contains "$IDENTITY_HELPER" 'COS_EXT_UID_FIRST=61000' \
    "package identity range must start below systemd DynamicUser"
assert_contains "$IDENTITY_HELPER" 'COS_EXT_GID=60999' \
    "fresh installs must prefer the fixed reserved gid"
assert_contains "$IDENTITY_HELPER" 'identity_select_gid' \
    "upgrades must safely retain a provable legacy package gid"
assert_contains "$IDENTITY_HELPER" 'COS_EXT_DYNAMIC_UID_FIRST=61184' \
    "package policy must encode systemd DynamicUser boundaries"
assert_contains "$IDENTITY_HELPER" '/proc/self/mountinfo' \
    "execution-gid validation must enumerate the live mount topology"
assert_contains "$IDENTITY_HELPER" '/usr/lib/cos/extension-gid-scan.py' \
    "identity provisioning must invoke the unpacked root-owned scan helper"
assert_contains "$BUILD_DEBS" 'extension-gid-scan.py' \
    "claw-os-agent must install its mount-pinning gid scan helper"
assert_contains "$PROJECT_DIR/packaging/deb/claw-os-agent/extension-gid-scan.py" \
    '"/usr/bin/getfacl",' \
    "the gid scan helper must use the real numeric getfacl interface"
assert_contains "$IDENTITY_RS" 'FIRST_UID: u32 = 61_000' \
    "runtime and packaged uid-range start must agree"
assert_contains "$IDENTITY_RS" 'GROUP_GID: u32 = 60_999' \
    "runtime and packaged gid must agree"
assert_contains "$IDENTITY_RS" 'IDENTITY_COUNT: u32 = 64' \
    "runtime and packaged identity count must agree"

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
grep -Eq '^KillMode=control-group$' "$UNIT" ||
    fail "clawd.service must kill delegated extension cgroups on daemon stop"
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
grep -Eq '^Group=root$' "$UNIT" ||
    fail "clawd.service must create runtime and state roots as root:root"
grep -Eq '^Environment=CLAWD_SOCKET_GROUP=sudo$' "$UNIT" ||
    fail "clawd.service must preserve root:sudo ownership for the primary socket"

assert_contains "$TEST_WORKFLOW" 'Run privileged extension boundaries' \
    "CI must execute the root extension boundary suites"
assert_contains "$TEST_WORKFLOW" 'install -o root -g root -m 0755' \
    "privileged test binaries must use root-owned non-writable fixtures"
assert_contains "$TEST_WORKFLOW" 'COS_PRIVILEGED_EXTENSION_HOST_BIN' \
    "installed-system tests must use the root-owned extension host fixture"
assert_contains "$TEST_WORKFLOW" '"$root/extension_host_boundary" --test-threads=1' \
    "CI must run the repaired extension-host lifecycle test"
assert_contains "$TEST_WORKFLOW" 'COS_REQUIRE_PRIVILEGED_CHILD_TESTS=1' \
    "CI must fail when a root-gated child-isolation test skips"
assert_contains "$TEST_WORKFLOW" 'extension_host::child_isolation::tests' \
    "CI must run the root-gated child-isolation library tests"
assert_contains "$TEST_WORKFLOW" 'expected_privileged_child_tests=19' \
    "CI must explicitly account for every privileged child-isolation test"
assert_contains "$TEST_WORKFLOW" '"$lib_test" "$root/cos_lib_tests"' \
    "CI must copy the lib test binary into the root-owned fixture"
assert_contains "$TEST_WORKFLOW" 'COS_GID_SCAN_HELPER_SOURCE="$root/extension-gid-scan.py"' \
    "CI must run the real gid helper from the root-owned fixture"
assert_contains "$TEST_WORKFLOW" '/usr/bin/python3 "$root/test-extension-gid-scan.py"' \
    "CI must execute real getfacl, mount pin, and timeout integration tests"

bash "$SCRIPT_DIR/test-extension-identities.sh"

printf 'ok - agent isolation packaging contract\n'
