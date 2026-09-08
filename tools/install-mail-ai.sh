#!/usr/bin/env bash
# tools/install-mail-ai.sh
#
# Install the Claw Mail AI WebExtension and Native Messaging registration
# for Thunderbird. The shared App, SDK and trusted launcher must already be
# installed together by claw-os-agent; this script never overwrites them.
#
#   - WebExtension XPI          → /usr/lib/thunderbird/distribution/extensions/ (package-owned)
#   - Thunderbird policies      → /etc/thunderbird/policies/policies.json
#   - Thunderbird NM manifest   → /etc/thunderbird/native-messaging-hosts/os.claw.mail_ai.json
#   - Native host launcher      → /usr/lib/cos/claw-mail-ai-host (package-owned)
#   - Shared App + SDK/runtime   → /usr/lib/cos/apps/mail-ai/, /usr/lib/cos/python/ (package-owned)
#
# Run as root. Re-run is idempotent (overwrites).
#
# After install, restart Thunderbird. The extension is force-installed
# via managed policy and loads on first launch of each profile.

set -euo pipefail

if [[ "${EUID}" -ne 0 ]]; then
  echo "must run as root" >&2
  exit 1
fi

EXT_ID="claw-mail-ai@claw.os"
APP_DEST="/usr/lib/cos/apps/mail-ai"
SDK_DEST="/usr/lib/cos/python/claw_os_sdk"
RUNTIME_DEST="/usr/lib/cos/python/cos_runtime"
HOST_LAUNCHER="/usr/lib/cos/claw-mail-ai-host"
NM_MANIFEST="/etc/thunderbird/native-messaging-hosts/os.claw.mail_ai.json"
POLICY_FILE="/etc/thunderbird/policies/policies.json"
XPI_DEST_DIR="/usr/lib/thunderbird/distribution/extensions"
XPI_DEST="${XPI_DEST_DIR}/${EXT_ID}.xpi"

# Sanity checks.
for d in "${APP_DEST}" "${SDK_DEST}" "${RUNTIME_DEST}"; do
  if [[ ! -d "$d" ]]; then
    echo "error: required extension/package directory missing: $d; install claw-os-agent first" >&2
    exit 1
  fi
done
if [[ ! -f "${XPI_DEST}" ]]; then
  echo "error: package-owned Mail extension missing: ${XPI_DEST}; install claw-os-agent first" >&2
  exit 1
fi
for binary in /usr/local/bin/claw-app-runner /usr/bin/python3 "${HOST_LAUNCHER}"; do
  if [[ ! -x "${binary}" ]]; then
    echo "error: required executable missing: ${binary}" >&2
    exit 1
  fi
done
for file in app.json main.py server.py native_host.py; do
  if [[ ! -f "${APP_DEST}/${file}" ]]; then
    echo "error: claw-os-agent Mail package is incomplete: ${APP_DEST}/${file}" >&2
    exit 1
  fi
done

# ---------------------------------------------------------------------------
# 1. Reuse the shared, authenticated App package, SDK and UI protocol adapter.
# ---------------------------------------------------------------------------
echo "[claw-mail-ai] using package-owned Mail implementation → ${APP_DEST}"
echo "[claw-mail-ai] using package-owned Mail extension → ${XPI_DEST}"

# ---------------------------------------------------------------------------
# 2. Native Messaging manifest for the package-owned launcher.
# ---------------------------------------------------------------------------
echo "[claw-mail-ai] installing NM manifest → ${NM_MANIFEST}"
install -d -m 0755 "$(dirname "${NM_MANIFEST}")"
cat > "${NM_MANIFEST}" <<EOF
{
  "name": "os.claw.mail_ai",
  "description": "Claw Mail AI — Native Messaging bridge between Thunderbird and apps/mail-ai",
  "path": "${HOST_LAUNCHER}",
  "type": "stdio",
  "allowed_extensions": ["${EXT_ID}"]
}
EOF
chmod 0644 "${NM_MANIFEST}"

# ---------------------------------------------------------------------------
# 3. Thunderbird policy — force-install the extension.
#
# We MERGE into any existing policies.json instead of clobbering it. The
# previous behavior overwrote site-admin or distro-supplied policy keys
# (DownloadDirectory, AppUpdateURL, …) every time the script ran. Now we
# read what's there, layer our keys on top, and write the union.
# ---------------------------------------------------------------------------
echo "[claw-mail-ai] installing policy      → ${POLICY_FILE}"
install -d -m 0755 "$(dirname "${POLICY_FILE}")"

OUR_POLICY=$(cat <<EOF
{
  "policies": {
    "ExtensionSettings": {
      "${EXT_ID}": {
        "installation_mode": "force_installed",
        "install_url": "file://${XPI_DEST}",
        "default_area": "navbar"
      }
    },
    "DisableTelemetry": true,
    "DisableAppUpdate": false,
    "BlockAboutConfig": false
  }
}
EOF
)

merge_policy() {
  local existing="$1"
  local ours="$2"
  if command -v jq >/dev/null 2>&1; then
    # Deep-merge: ours wins on collisions, but keys we don't touch
    # (e.g. existing site policies) are preserved.
    jq -s '.[0] * .[1]' <(printf '%s' "$existing") <(printf '%s' "$ours")
  else
    /usr/bin/env python3 - "$existing" "$ours" <<'PY'
import json, sys
def deep_merge(a, b):
    if isinstance(a, dict) and isinstance(b, dict):
        out = dict(a)
        for k, v in b.items():
            out[k] = deep_merge(a.get(k), v) if k in a else v
        return out
    return b
existing = json.loads(sys.argv[1]) if sys.argv[1].strip() else {}
ours = json.loads(sys.argv[2])
print(json.dumps(deep_merge(existing, ours), indent=2))
PY
  fi
}

EXISTING_POLICY="{}"
if [[ -f "${POLICY_FILE}" ]]; then
  # If the file is unparseable, back it up and start from {} — we never
  # want a broken policies.json to brick Thunderbird on the next launch.
  if /usr/bin/env python3 -c "import json,sys; json.load(open(sys.argv[1]))" "${POLICY_FILE}" 2>/dev/null; then
    EXISTING_POLICY="$(cat "${POLICY_FILE}")"
  else
    echo "warning: ${POLICY_FILE} is not valid JSON — backing up to ${POLICY_FILE}.bak" >&2
    cp -a "${POLICY_FILE}" "${POLICY_FILE}.bak"
  fi
fi

MERGED_POLICY="$(merge_policy "${EXISTING_POLICY}" "${OUR_POLICY}")"
printf '%s\n' "${MERGED_POLICY}" > "${POLICY_FILE}"
chmod 0644 "${POLICY_FILE}"

cat <<MSG

[claw-mail-ai] install complete.

Verify:
  1. Restart Thunderbird.
  2. Look for the Claw AI button on the toolbar and a "Mail Assistant"
     entry in the Spaces sidebar.
  3. Open a message — click the Claw AI button in the message header
     to see a Summary popup.
  4. Open Tools → Add-ons and Themes → check that "Claw Mail AI" is
     installed and enabled.
  5. To debug the Native Messaging bridge:
       journalctl --user -t thunderbird
       /usr/bin/python3 ${APP_DEST}/native_host.py --probe

If the extension doesn't appear, confirm:
  - ${XPI_DEST} exists and is readable
  - ${POLICY_FILE} is parseable JSON
  - The Thunderbird policy mechanism is honored (>=68): check
    Help → Troubleshooting Information → Important Modified Preferences

MSG
