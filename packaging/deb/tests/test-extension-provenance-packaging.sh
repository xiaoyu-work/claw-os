#!/bin/bash
# packaging/deb/tests/test-extension-provenance-packaging.sh -- the
# installed system must be able to authenticate extension packages.
#
# Apps, Skills and MCP/adapter packages are only trusted after their
# `claw.provenance/v1` envelope verifies against a trusted publisher
# key, or after they are recognised as root-owned package content. That
# turns two things into packaging contracts:
#
#   * the trust roots must exist with root ownership and without group
#     or world write bits, otherwise the loader refuses them and every
#     user-installed extension is quarantined on a fresh install; and
#   * no private signing key may ever be shipped.
#
# Both are checked here so the packaging cannot drift away from what the
# verifier demands at runtime.

set -euo pipefail

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PROJECT_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)

BUILD="$PROJECT_DIR/packaging/deb/build-debs.sh"
TRUST_MODULE="$PROJECT_DIR/core/src/provenance/trust.rs"
VERIFY_MODULE="$PROJECT_DIR/core/src/provenance/verify.rs"
VENDOR_TRUST_DIR="$PROJECT_DIR/packaging/deb/claw-os-agent/trust/publishers.d"

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

# The roots the verifier reads must be the roots the package creates.
for root in /usr/lib/cos/trust/publishers.d /etc/cos/trust/publishers.d; do
    assert_contains "$TRUST_MODULE" "$root" \
        "the trust loader must know about $root"
    assert_contains "$BUILD" "$root" \
        "claw-os-agent must create $root"
done

# Vendor package roots the verifier accepts must be the ones the package
# actually installs into.
for root in /usr/lib/cos /usr/share/claw; do
    assert_contains "$VERIFY_MODULE" "\"$root\"" \
        "the verifier must list $root as an approved package root"
done

# Modes: a group- or world-writable trust root contributes nothing, so
# shipping one would silently disable publisher trust.
assert_contains "$BUILD" 'chmod 0755 \' \
    "the build must set explicit modes on the trust roots"
assert_contains "$BUILD" 'install -m 0644 "$trust_file"' \
    "vendor trust entries must be installed world-readable but not writable"

# Private key material must never be shipped.
assert_contains "$BUILD" '"private_key"' \
    "the build must refuse a vendor trust file containing private key material"

# The vendor store is either intentionally empty (fail closed: nothing
# third-party is trusted until an operator installs a key) or it holds
# real, structurally valid public keys. A placeholder, a key with no
# accountable owner, or an entry that is not a well-formed
# `claw.trust/v1` document is worse than nothing: it manufactures a
# trusted signer.
vendor_entries=0
if [ -d "$VENDOR_TRUST_DIR" ]; then
    while IFS= read -r -d '' entry; do
        vendor_entries=$((vendor_entries + 1))
        if grep -q '"private_key"' "$entry"; then
            fail "vendor trust file ships private key material: $entry"
        fi
        if ! grep -q '"schema"[[:space:]]*:[[:space:]]*"claw.trust/v1"' "$entry"; then
            fail "vendor trust file must declare schema claw.trust/v1: $entry"
        fi
        if ! grep -q '"algorithm"[[:space:]]*:[[:space:]]*"ed25519"' "$entry"; then
            fail "vendor trust file must declare algorithm ed25519: $entry"
        fi
        # A key id is a digest of the key material, so both must be
        # present and correctly shaped; a 64-hex public key and a
        # sha256: id are the minimum the loader will accept.
        if ! grep -Eq '"public_key"[[:space:]]*:[[:space:]]*"[0-9a-f]{64}"' "$entry"; then
            fail "vendor trust file has no 64-hex ed25519 public key: $entry"
        fi
        if ! grep -Eq '"key_id"[[:space:]]*:[[:space:]]*"sha256:[0-9a-f]{64}"' "$entry"; then
            fail "vendor trust file has no sha256: key id: $entry"
        fi
        # Refuse obvious stand-ins.
        if grep -Eqi '(placeholder|example[-_ ]?key|changeme|todo|fixme|dummy|test[-_ ]?key)' "$entry"; then
            fail "vendor trust file looks like a placeholder: $entry"
        fi
        if grep -Eq '"public_key"[[:space:]]*:[[:space:]]*"(0{64}|f{64}|(aa|bb|cc|dd|ee|11|22)+)"' "$entry"; then
            fail "vendor trust file carries a patterned stand-in key: $entry"
        fi
    done < <(find "$VENDOR_TRUST_DIR" -name '*.json' -type f -print0)
fi

if [ "$vendor_entries" -eq 0 ]; then
    # Empty is a supported, documented state — but it has to be the
    # *deliberate* one, so the README must say so and the vendor path
    # must still be reachable through package-manager trust.
    if [ ! -f "$VENDOR_TRUST_DIR/README.md" ]; then
        fail "empty vendor trust store must document that it is intentionally empty"
    fi
    if ! grep -q 'ships empty, on purpose' "$VENDOR_TRUST_DIR/README.md"; then
        fail "empty vendor trust store must state that it is intentional and fail-closed"
    fi
    assert_contains "$VERIFY_MODULE" 'VENDOR_PACKAGE_ROOTS' \
        "built-in content must still be trusted through root-owned package roots"
fi

# No document anywhere may claim a publisher is trusted out of the box.
while IFS= read -r -d '' doc; do
    if grep -Eqi 'default (publisher|signing) key is (installed|trusted|shipped)' "$doc"; then
        fail "documentation claims a default publisher is trusted: $doc"
    fi
done < <(find "$PROJECT_DIR/docs" "$PROJECT_DIR/packaging" -name '*.md' -type f -print0 2>/dev/null)

# Nothing in the tree may carry a signing key.
while IFS= read -r -d '' candidate; do
    case "$candidate" in
        */target/*|*/build/*) continue ;;
    esac
    fail "repository contains signing key material: $candidate"
done < <(grep -rlZ --include='*.json' '"schema"[[:space:]]*:[[:space:]]*"claw.signing-key/v1"' \
    "$PROJECT_DIR" 2>/dev/null || true)

# Verification must not be reachable from the environment: a packaged
# system has to fail closed regardless of what is exported to it.
if grep -Eq 'env::var[^)]*"COS_(TRUST|PROVENANCE)' "$TRUST_MODULE" "$VERIFY_MODULE"; then
    fail "no environment variable may add a trust root or disable verification"
fi

# Long-lived daemons must be able to notice a revocation. The durable
# per-domain state file is what makes that cheap; without it a running
# clawd would keep honouring a key the operator revoked.
STATE_MODULE="$PROJECT_DIR/core/src/provenance/state.rs"
assert_contains "$STATE_MODULE" 'claw.trust-state/v1' \
    "the durable trust generation format must be versioned"
assert_contains "$TRUST_MODULE" 'pub fn is_current' \
    "the trust store must expose a cheap staleness check for daemons"

# Unsigned code may only be trusted through an interactive human
# decision, never a flag.
CONSENT_MODULE="$PROJECT_DIR/core/src/provenance/consent.rs"
assert_contains "$CONSENT_MODULE" 'NotInteractive' \
    "developer trust must refuse a non-interactive process"
if grep -Eq 'auto_yes[[:space:]]*=>[[:space:]]*(return[[:space:]]+)?Ok' "$CONSENT_MODULE"; then
    fail "--yes must never satisfy developer trust"
fi

printf 'ok - extension provenance packaging contract\n'
