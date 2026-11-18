#!/usr/bin/env bash
set -euo pipefail

project="${1:?source checkout required}"
stage="${2:?package staging path required}"
work="${3:?build directory required}"
jobs="${CMAKE_BUILD_PARALLEL_LEVEL:-4}"

export PATH="/root/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export CARGO_TARGET_DIR="$work/cargo"
rustup toolchain install stable --profile minimal

cargo +stable build --manifest-path "$project/Cargo.toml" --locked --release -p claw-compositor
cargo +stable build --manifest-path "$project/desktop/agent/Cargo.toml" --locked --release --workspace
cargo +stable build --manifest-path "$project/cos-runtime/rust/Cargo.toml" --locked --release \
    --bin cos-ask-claw-launcher
cargo +stable build --manifest-path "$project/desktop/services/claw-applet-services/Cargo.toml" \
    --locked --release --features provider --bin claw-os-applet-provider

cp -a "$project/desktop/hyprland" "$project/desktop/hyprland-claw" "$work/"
python3 "$project/desktop/hyprland-claw/build-dependencies.py" --cache "$work/deps" --jobs "$jobs"
prefix="$work/deps/prefix"
export PATH="$prefix/bin:$PATH"
export CMAKE_PREFIX_PATH="$prefix"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig:$prefix/share/pkgconfig"
cmake -S "$work/hyprland" -B "$work/compositor" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr \
    "-DCLAW_COMPOSITOR_LIBRARY=$CARGO_TARGET_DIR/release/libclaw_compositor.a" \
    -DNO_UWSM=ON -DBUILD_TESTING=OFF
cmake --build "$work/compositor" --parallel "$jobs" --target Hyprland

native="$stage/usr/libexec/claw-hyprland"
install -Dm0755 "$work/compositor/Hyprland" "$native/Hyprland"
install -d "$native/lib"
cp -a "$prefix"/lib/*.so.* "$native/lib/"
patchelf --set-rpath '$ORIGIN/lib' "$native/Hyprland"
find "$native/lib" -type f -name '*.so.*' -exec patchelf --set-rpath '$ORIGIN' {} +
if find "$native/lib" -xtype l -print -quit | grep -q .; then
    echo "error: staged Hyprland has a broken shared-library link" >&2
    exit 1
fi

share="$stage/usr/share/claw-hyprland"
install -d "$share"
cp -a "$prefix/share/libinput" "$share/libinput"
license="$stage/usr/share/doc/claw-os-desktop"
install -Dm0644 "$work/hyprland/LICENSE" "$license/hyprland/LICENSE"
install -Dm0644 "$work/hyprland/subprojects/udis86/LICENSE" "$license/hyprland/LICENSE-udis86"
install -Dm0644 /usr/share/common-licenses/GPL-3 "$license/hyprland/LICENSE-claw-authority"
install -Dm0644 "$work/hyprland-claw/dependencies.lock.json" "$license/hyprland/sources.json"
for source in "$work"/deps/sources/*; do
    while IFS= read -r -d '' notice; do
        install -Dm0644 "$notice" "$license/hyprland/${source##*/}/${notice##*/}"
    done < <(find "$source" -maxdepth 1 -type f \
        \( -iname 'license*' -o -iname 'copying*' -o -iname 'copyright*' \) -print0)
done
install -Dm0644 "$work/deps/sources/lua-5.5.1/doc/readme.html" "$license/hyprland/lua/LICENSE.html"

for binary in cos-agent-ui cos-agent-bridge cos-ask-claw-launcher; do
    install -Dm0755 "$CARGO_TARGET_DIR/release/$binary" "$stage/usr/local/bin/$binary"
done
install -Dm0755 "$CARGO_TARGET_DIR/release/claw-os-applet-provider" \
    "$stage/usr/libexec/claw-os-applet-provider"
install -Dm0644 "$project/desktop/services/claw-applet-services/LICENSE" "$license/applet-services.LICENSE"

session="$work/hyprland-claw/session"
install -Dm0644 "$session/claw.desktop" "$stage/usr/share/wayland-sessions/claw.desktop"
install -Dm0644 "$session/claw-desktop-session.target" "$stage/usr/lib/systemd/user/claw-desktop-session.target"
install -Dm0644 "$session/claw-desktop.pam" "$stage/etc/pam.d/claw-desktop"
install -Dm0644 "$session/greetd.toml" "$stage/etc/greetd/claw-desktop.toml"
install -Dm0644 "$session/greetd.conf" "$stage/usr/lib/systemd/system/greetd.service.d/claw-desktop.conf"
install -Dm0644 "$session/hyprland-portals.conf" "$stage/usr/share/xdg-desktop-portal/hyprland-portals.conf"

# Query the target distro's shlibs database, not that of the image builder.
cd "$work"
mkdir -p debian
printf 'Source: claw-os\n\nPackage: claw-os-desktop\nArchitecture: any\nDescription: Claw OS Desktop\n' >debian/control
mapfile -d '' libraries < <(find "$native/lib" -type f -name '*.so.*' -print0)
for library in "${libraries[@]}"; do
    objdump -p "$library" | sed -nE 's/^[[:space:]]+SONAME[[:space:]]+(.+)\.so\.(.+)$/\1 \2 claw-os-desktop/p'
done >debian/shlibs.local
dpkg-shlibdeps -O -l"$native/lib" -xclaw-os-desktop "$native/Hyprland" "${libraries[@]}" \
    "$stage/usr/local/bin/cos-agent-ui" "$stage/usr/local/bin/cos-agent-bridge" \
    "$stage/usr/local/bin/cos-ask-claw-launcher" "$stage/usr/libexec/claw-os-applet-provider" \
    | sed -n 's/^shlibs:Depends=//p' >"$work/runtime-depends"
test -s "$work/runtime-depends"
