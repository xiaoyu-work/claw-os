#!/bin/sh
set -eu
umask 077
export LC_ALL=C

fail() {
    printf 'Claw OS: %s\n' "$*" >&2
    exit 1
}

[ "$#" -eq 3 ] || fail "usage: /bin/sh $0 OUTPUT_DIRECTORY DEVELOPER_ID TEAM_ID"
[ "$(/usr/bin/uname -s)" = Darwin ] && [ "$(/usr/bin/uname -m)" = arm64 ] \
    || fail "build on Apple Silicon macOS with Xcode"
[ "$(/usr/bin/id -u)" -ne 0 ] || fail "build as the developer with the signing identity in their keychain, not root"

scripts=$(cd "$(/usr/bin/dirname "$0")" && pwd)
. "$scripts/macos-restore-source.sh"
output=$1
case "$output" in
    /*) ;;
    *) output="$PWD/$output" ;;
esac
case "$2" in
    'Developer ID Application: '*) ;;
    *) fail "use a Developer ID Application signing identity" ;;
esac
requirement=$(restore_requirement "$3") || fail "use the ten-character Apple Team ID"

work=$(/usr/bin/mktemp -d "${TMPDIR:-/tmp}/claw-restore.XXXXXX")
trap '/bin/rm -rf -- "$work"' EXIT
fetch_pinned "$NEO_SOURCE_URL" "$NEO_SOURCE_SHA256" "$work/source.tar.gz" \
    || fail "could not verify the pinned Neo restore helper source"
package="$NEO_SOURCE_ROOT/Engine/RestoreImage"
/usr/bin/tar -oxzf "$work/source.tar.gz" -C "$work" \
    "$package/Package.swift" "$package/Sources" "$package/Tests" "$package/LICENSE"
package="$work/$package"

swift_build() {
    /usr/bin/xcrun swift "$@" --package-path "$package" --scratch-path "$work/build" \
        --configuration release --triple arm64-apple-macosx14.0
}
swift_build test
swift_build build
binary_dir=$(swift_build build --show-bin-path)
helper="$work/claw-os-restore-image"
/usr/bin/install -m 0755 "$binary_dir/omarchy-restore-image" "$helper"
/usr/bin/codesign --force --options runtime --timestamp --identifier "$RESTORE_IDENTIFIER" --sign "$2" "$helper"
/usr/bin/codesign --verify --strict "-R=$requirement" "$helper"
"$helper" capabilities

/bin/mkdir "$output"
/usr/bin/install -m 0755 "$helper" "$output/claw-os-restore-image"
/usr/bin/install -m 0644 "$package/LICENSE" "$output/LICENSE"
(
    cd "$output"
    /usr/bin/shasum -a 256 claw-os-restore-image | /usr/bin/tee claw-os-restore-image.sha256
)
printf 'Apple Team ID: %s\n' "$3"
