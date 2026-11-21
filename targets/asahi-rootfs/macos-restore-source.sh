NEO_SOURCE_REVISION=f8bad2d2ff1cb3344b4282f44e75a4a25c127ea9
NEO_SOURCE_SHA256=abd5bada89bab9d9b4291b4c2a217e9f2ab1b0c61b73fb078959908321bfe26a
NEO_SOURCE_URL="https://codeload.github.com/omacom/omarchy-mac-installer/tar.gz/$NEO_SOURCE_REVISION"
NEO_SOURCE_ROOT="omarchy-mac-installer-$NEO_SOURCE_REVISION"
RESTORE_IDENTIFIER=org.yuos.installer.restore-image

valid_sha256() {
    case "$1" in
        *[!0-9a-f]*) return 1 ;;
    esac
    [ "${#1}" -eq 64 ]
}

verify_sha256() {
    printf '%s  %s\n' "$1" "$2" | /usr/bin/env -i PATH=/usr/bin:/bin /usr/bin/shasum -a 256 -c - >/dev/null
}

fetch_pinned() {
    /usr/bin/curl -q -fsSL --proto '=https' --proto-redir '=https' --output "$3" "$1" &&
        verify_sha256 "$2" "$3"
}

restore_requirement() {
    case "$1" in
        *[!A-Z0-9]*) return 1 ;;
    esac
    [ "${#1}" -eq 10 ] || return 1
    printf '%s\n' "anchor apple generic and identifier \"$RESTORE_IDENTIFIER\" and certificate 1[field.1.2.840.113635.100.6.2.6] and certificate leaf[field.1.2.840.113635.100.6.1.13] and certificate leaf[subject.OU] = \"$1\""
}
