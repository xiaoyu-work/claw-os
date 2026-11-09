#!/usr/bin/env bash
# tools/install-browser-agent.sh
#
# Install the Claw Agent WebExtension and its Native Messaging host system-wide.
#
#   - WebExtension source       → /usr/share/claw/extensions/claw-agent-browser/
#   - OS transport (Python)     → /usr/lib/cos/browser-native-host.py
#   - Chromium managed policy   → /etc/chromium/policies/managed/claw-agent.json
#   - Chromium NM host manifest → /etc/chromium/native-messaging-hosts/com.clawos.browser.json
#   - Native host launcher      → /usr/lib/cos/claw-browser-host
#
# Run as root.  Re-run is idempotent (overwrites).
#
# Configure already installed product assets and the versioned OS transport.
# This script does not fetch, copy or replace App package files.

set -euo pipefail

if [[ "${EUID}" -ne 0 ]]; then
  echo "must run as root" >&2
  exit 1
fi

EXT_DEST="/usr/share/claw/extensions/claw-agent-browser"
HOST_LAUNCHER="/usr/lib/cos/claw-browser-host"
HOST_IMPL="/usr/lib/cos/browser-native-host.py"
NM_MANIFEST="/etc/chromium/native-messaging-hosts/com.clawos.browser.json"
POLICY_FILE="/etc/chromium/policies/managed/claw-agent.json"

# Extension ID is deterministic when packed with a known key, but for the MVP
# we install the unpacked extension for the administrator to load explicitly.
# Callers can override CLAW_EXT_ID once they generate a stable packed CRX.
EXT_ID="${CLAW_EXT_ID:-}"

if [[ -n "${EXT_ID}" && ! "${EXT_ID}" =~ ^[a-p]{32}$ ]]; then
  echo "CLAW_EXT_ID must be the Chromium extension's 32-letter ID" >&2
  exit 1
fi

for file in "${HOST_IMPL}" "${HOST_LAUNCHER}" \
  "${EXT_DEST}/manifest.json" "${EXT_DEST}/background.js" \
  "${EXT_DEST}/content.js" "${EXT_DEST}/popup.html" "${EXT_DEST}/popup.js"; do
  if [[ ! -f "${file}" || -L "${file}" ]]; then
    echo "missing installed Browser asset: ${file}; install claw-app-browser and the matching claw-os-agent first" >&2
    exit 1
  fi
  mode="$(stat -c %a "${file}")"
  if [[ "$(stat -c %u "${file}")" != 0 ]] || (( (8#${mode} & 8#022) != 0 )); then
    echo "installed Browser assets must be root-owned and not group/world writable: ${file}" >&2
    exit 1
  fi
done
if [[ ! -x "${HOST_LAUNCHER}" ]]; then
  echo "installed Browser launcher is not executable: ${HOST_LAUNCHER}" >&2
  exit 1
fi

echo "[claw] installing Native Messaging manifest → ${NM_MANIFEST}"
install -d -m 0755 "$(dirname "${NM_MANIFEST}")"
if [[ -n "${EXT_ID}" ]]; then
  ALLOWED_ORIGINS=$(printf '"chrome-extension://%s/"' "${EXT_ID}")
else
  # No stable ID yet → leave allowed_origins empty.  After the first load the
  # admin/installer must edit this file with the extension's runtime ID
  # (chrome://extensions shows it), then restart Chromium.
  ALLOWED_ORIGINS=""
fi
cat > "${NM_MANIFEST}" <<EOF
{
  "name": "com.clawos.browser",
  "description": "Claw Agent — bridge between Chromium and the cos kernel",
  "path": "${HOST_LAUNCHER}",
  "type": "stdio",
  "allowed_origins": [${ALLOWED_ORIGINS}]
}
EOF
chmod 0644 "${NM_MANIFEST}"

echo "[claw] installing Chromium managed policy → ${POLICY_FILE}"
install -d -m 0755 "$(dirname "${POLICY_FILE}")"
# Pin force-install / allowlist to the specific extension ID. If the
# caller has not yet generated a stable ID, we ship an empty allowlist
# (which disables *all* extension installs via policy) — strictly safer
# than the previous "*" wildcard, which let any side-loaded extension
# in. The admin re-runs the script with CLAW_EXT_ID=<id> once known.
if [[ -n "${EXT_ID}" ]]; then
  POLICY_ALLOWLIST=$(printf '"%s"' "${EXT_ID}")
  POLICY_EXT_SETTINGS=$(printf '"%s": { "installation_mode": "allowed" }' "${EXT_ID}")
else
  POLICY_ALLOWLIST=""
  POLICY_EXT_SETTINGS=""
fi
cat > "${POLICY_FILE}" <<EOF
{
  "ExtensionSettings": {
    "*": { "installation_mode": "blocked" }$( [[ -n "${POLICY_EXT_SETTINGS}" ]] && printf ',\n    %s' "${POLICY_EXT_SETTINGS}" )
  },
  "ExtensionInstallAllowlist": [${POLICY_ALLOWLIST}]
}
EOF
chmod 0644 "${POLICY_FILE}"

cat <<MSG

[claw] install complete.

Next steps:
  1. Launch Chromium.
  2. chrome://extensions → Developer mode ON → Load unpacked → ${EXT_DEST}
  3. Note the assigned Extension ID (32 lowercase letters).
  4. Re-run this script with CLAW_EXT_ID=<id> to pin Native Messaging
     and the extension allowlist to that ID:
       CLAW_EXT_ID=<id> sudo ${BASH_SOURCE[0]}
  5. Restart Chromium.

To verify the bridge:
  ls -l \$XDG_RUNTIME_DIR/claw-browser.sock     # socket should exist
  cos app browser-attached tabs.list            # approval-gated tab list

MSG
