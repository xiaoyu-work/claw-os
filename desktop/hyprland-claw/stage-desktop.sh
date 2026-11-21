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

source_adapter="$project/desktop/hyprland-claw"
bash "$source_adapter/build-locker.sh" "$stage" "$work"
compiler="$(python3 "$source_adapter/build-toolchain.py" "$work")"
export CC="$compiler/bin/gcc" CXX="$compiler/bin/g++"
export LD_LIBRARY_PATH="$compiler/lib64:$compiler/lib"
toolchain="$("$CXX" -dumpfullversion)"
dependencies="$work/deps-gcc-$toolchain"
cmake_build="$work/compositor-gcc-$toolchain"
cp -a "$project/desktop/hyprland" "$source_adapter" "$work/"
python3 "$source_adapter/build-dependencies.py" --cache "$dependencies" --jobs "$jobs"
prefix="$dependencies/prefix"
export PATH="$prefix/bin:$PATH"
export CMAKE_PREFIX_PATH="$prefix"
export PKG_CONFIG_PATH="$prefix/lib/pkgconfig:$prefix/share/pkgconfig"
cmake --fresh -S "$work/hyprland" -B "$cmake_build" -G Ninja \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr \
    -DPKG_CONFIG_EXECUTABLE=/usr/bin/env \
    '-DPKG_CONFIG_ARGN=-u;PKG_CONFIG_ALLOW_SYSTEM_LIBS;/usr/bin/pkg-config' \
    "-DCLAW_COMPOSITOR_LIBRARY=$CARGO_TARGET_DIR/release/libclaw_compositor.a" \
    -DNO_UWSM=ON -DBUILD_TESTING=OFF
cmake --build "$cmake_build" --parallel "$jobs" --target Hyprland

native="$stage/usr/libexec/claw-hyprland"
install -Dm0755 "$cmake_build/Hyprland" "$native/Hyprland"
install -d "$native/lib"
cp -a "$prefix"/lib/*.so.* "$native/lib/"
# Use the same newer C++ runtime as the build, private to the compositor.
for library in libstdc++.so.6 libgcc_s.so.1; do
    file="$("$CXX" "-print-file-name=$library")"
    test "${file#"$compiler/"}" != "$file"
    install -m0644 "$file" "$native/lib/$library"
done
unset LD_LIBRARY_PATH
patchelf --set-rpath '$ORIGIN/lib' "$native/Hyprland"
find "$native/lib" -type f -name '*.so.*' -exec patchelf --set-rpath '$ORIGIN' {} +
if find "$native/lib" -xtype l -print -quit | grep -q .; then
    echo "error: staged Hyprland has a broken shared-library link" >&2
    exit 1
fi

share="$stage/usr/share/claw-hyprland"
install -d "$share"
cp -a "$prefix/share/libinput" "$share/libinput"
hardware="$project/packaging/deb/claw-os-base/hardware"
install -m0644 "$hardware/99-claw-os-hardware.quirks" "$share/libinput/"
license="$stage/usr/share/doc/claw-os-desktop"
install -Dm0644 "$hardware/omarchy.LICENSE" "$license/omarchy-hardware.LICENSE"
install -Dm0644 "$work/hyprland/LICENSE" "$license/hyprland/LICENSE"
install -Dm0644 "$work/hyprland/subprojects/udis86/LICENSE" "$license/hyprland/LICENSE-udis86"
install -Dm0644 /usr/share/common-licenses/GPL-3 "$license/hyprland/LICENSE-claw-authority"
install -Dm0644 "$work/hyprland-claw/dependencies.lock.json" "$license/hyprland/sources.json"
install -Dm0644 "$source_adapter/toolchain.lock.json" "$license/hyprland/gcc/sources.json"
install -m0644 "$compiler/claw-notices/"* "$license/hyprland/gcc/"
for source in "$dependencies"/sources/*; do
    while IFS= read -r -d '' notice; do
        install -Dm0644 "$notice" "$license/hyprland/${source##*/}/${notice##*/}"
    done < <(find "$source" -maxdepth 1 -type f \
        \( -iname 'license*' -o -iname 'copying*' -o -iname 'copyright*' \) -print0)
done
install -Dm0644 "$dependencies/sources/lua-5.5.1/doc/readme.html" "$license/hyprland/lua/LICENSE.html"

for binary in cos-agent-ui cos-agent-bridge cos-ask-claw-launcher; do
    install -Dm0755 "$CARGO_TARGET_DIR/release/$binary" "$stage/usr/local/bin/$binary"
done
install -Dm0755 "$project/tools/browser-chat-native-host" \
    "$stage/usr/lib/cos/browser-chat-native-host"
install -Dm0755 "$CARGO_TARGET_DIR/release/claw-os-applet-provider" \
    "$stage/usr/libexec/claw-os-applet-provider"
install -Dm0644 "$project/desktop/services/claw-applet-services/LICENSE" "$license/applet-services.LICENSE"

session="$work/hyprland-claw/session"
install -Dm0644 "$session/claw.desktop" "$stage/usr/share/wayland-sessions/claw.desktop"
install -Dm0644 "$session/hyprland.lua" "$stage/etc/xdg/hypr/hyprland.lua"
install -Dm0644 "$session/claw-desktop-session.target" "$stage/usr/lib/systemd/user/claw-desktop-session.target"
install -Dm0644 "$session/claw-screen-reader.service" "$stage/usr/lib/systemd/user/claw-screen-reader.service"
install -Dm0644 "$session/claw-desktop.pam" "$stage/etc/pam.d/claw-desktop"
install -Dm0644 "$session/desktop-session.env" "$stage/usr/share/claw-os/desktop-session.env"
install -Dm0755 "$session/claw-greeter" "$stage/usr/libexec/claw-greeter"
install -Dm0755 "$session/claw-desktop-login" "$stage/usr/libexec/claw-desktop-login"
for name in claw-system-settings claw-os-updates; do
    install -Dm0755 "$session/$name" "$stage/usr/libexec/$name"
done
install -Dm0644 "$session/org.yuos.desktop.settings.policy" \
    "$stage/usr/share/polkit-1/actions/org.yuos.desktop.settings.policy"
units="$stage/usr/lib/systemd/system"
for file in "$session"/claw-os-update-*.service "$session"/claw-os-update-*.timer; do
    install -Dm0644 "$file" "$units/${file##*/}"
done
install -d "$units/system-update.target.wants" "$units/timers.target.wants"
ln -s ../claw-os-update-offline.service "$units/system-update.target.wants/"
ln -s ../claw-os-update-automatic.timer "$units/timers.target.wants/"
install -Dm0755 "$session/claw-create-local-owner" "$stage/usr/libexec/claw-create-local-owner"
install -Dm0755 "$session/claw-desktop-locales" "$stage/usr/libexec/claw-desktop-locales"
install -Dm0755 "$session/claw-desktop-reviews" "$stage/usr/libexec/claw-desktop-reviews"
install -Dm0644 "$session/greetd.toml" "$stage/etc/greetd/claw-desktop.toml"
install -Dm0644 "$session/greetd.conf" "$stage/usr/lib/systemd/system/greetd.service.d/claw-desktop.conf"
install -Dm0644 "$session/claw-desktop-accounts.conf" \
    "$units/clawd.service.d/claw-desktop-accounts.conf"
install -Dm0644 "$session/claw-desktop-users.conf" "$stage/usr/lib/sysusers.d/claw-desktop-users.conf"
install -Dm0644 "$session/hyprland-portals.conf" "$stage/usr/share/xdg-desktop-portal/hyprland-portals.conf"
install -Dm0755 "$session/claw-oobe" "$stage/usr/libexec/claw-oobe"
for file in "$session"/oobe/*; do
    install -Dm0644 "$file" "$stage/usr/share/claw-os/oobe/${file##*/}"
done
for file in "$project"/assets/brand/avatars/*.png; do
    install -Dm0644 "$file" "$stage/usr/share/claw-os/avatars/${file##*/}"
done
install -Dm0644 "$project/assets/brand/clawos-icon-192.png" "$stage/usr/share/claw-os/oobe/logo.png"
install -Dm0644 "$project/assets/brand/wallpaper/claw-default.jpg" "$stage/usr/share/backgrounds/claw-os/claw-default.jpg"
install -Dm0644 "$project/assets/brand/wallpaper/claw-default-blurred.jpg" "$stage/usr/share/claw-os/oobe/wallpaper-blurred.jpg"
install -Dm0644 "$project/assets/brand/wallpaper/NOTICE" "$license/wallpaper.NOTICE"
install -Dm0644 "$project/assets/brand/wallpaper/LICENSE" "$license/wallpaper.LICENSE"
install -Dm0644 "$session/claw-driver-online.service" "$stage/usr/lib/systemd/system/claw-driver-online.service"
install -Dm0755 "$session/70-claw-driver-setup" "$stage/usr/lib/NetworkManager/dispatcher.d/70-claw-driver-setup"

# Query the target distro's shlibs database, not that of the image builder.
cd "$work"
mkdir -p debian
printf 'Source: claw-os\n\nPackage: claw-os-desktop\nArchitecture: any\nDescription: Claw OS Desktop\n' >debian/control
mapfile -d '' libraries < <(find "$native/lib" -type f -name '*.so.*' -print0)
for library in "${libraries[@]}"; do
    objdump -p "$library" | sed -nE 's/^[[:space:]]+SONAME[[:space:]]+(.+)\.so\.(.+)$/\1 \2 claw-os-desktop/p'
done >debian/shlibs.local
dpkg-shlibdeps -O -l"$native/lib" -xclaw-os-desktop "$native/Hyprland" "${libraries[@]}" \
    "$native/claw-lockscreen" \
    "$stage/usr/local/bin/cos-agent-ui" "$stage/usr/local/bin/cos-agent-bridge" \
    "$stage/usr/local/bin/cos-ask-claw-launcher" "$stage/usr/libexec/claw-os-applet-provider" \
    | sed -n 's/^shlibs:Depends=//p' >"$work/runtime-depends"
test -s "$work/runtime-depends"
