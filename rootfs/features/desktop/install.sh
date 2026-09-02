#!/usr/bin/env bash
# rootfs/features/desktop/install.sh — build the claw-os desktop from
# source (vendored under PROJECT_DIR/desktop) and wire it up so the rootfs
# boots into a Wayland login.
#
# Target distro: Debian 13 "trixie" (kernel 6.12 LTS, PipeWire 1.4, Mesa 24).
#
# Inputs (env):
#   ROOTFS       — target rootfs (from rootfs/build.sh)
#   PROJECT_DIR  — claw-os repo root (from rootfs/build.sh)
#   SCRIPT_DIR   — rootfs/ dir (from rootfs/build.sh)
#   DESKTOP_SRC  — optional override; otherwise $PROJECT_DIR/desktop
#
# Skipping: set DESKTOP_SKIP=1 to install runtime deps + overlay only and
# skip the ~30-60min cargo build. Useful when iterating on packages.txt /
# overlay / wiring without rebuilding the binaries.

set -euo pipefail

source "$PROJECT_DIR/scripts/lib/git-readonly.sh"

DESKTOP_SRC="${DESKTOP_SRC:-$PROJECT_DIR/desktop}"
FEATURE_DIR="$SCRIPT_DIR/features/desktop"
DESKTOP_PACKAGE_ROOT="$ROOTFS/build/claw-os-desktop-root"
DESKTOP_PACKAGE_ROOT_CHROOT="/build/claw-os-desktop-root"

# ---------------------------------------------------------------------------
# 0. Prepare package staging.
# ---------------------------------------------------------------------------
if [ "${DESKTOP_SKIP:-0}" = "1" ]; then
    if [ -d "$FEATURE_DIR/overlay" ] && [ -n "$(ls -A "$FEATURE_DIR/overlay" 2>/dev/null)" ]; then
        echo "  :: applying desktop overlay"
        cp -a --no-preserve=ownership "$FEATURE_DIR/overlay/." "$ROOTFS/"
    fi
    echo "  :: DESKTOP_SKIP=1 — runtime deps + overlay applied, build skipped"
    echo "  :: NOTE: login chain not wired (greeter binary missing). Re-run"
    echo "         without DESKTOP_SKIP to get a bootable graphical session."
    exit 0
fi

rm -rf "$DESKTOP_PACKAGE_ROOT"
mkdir -p "$DESKTOP_PACKAGE_ROOT"
if [ -d "$FEATURE_DIR/overlay" ] && [ -n "$(ls -A "$FEATURE_DIR/overlay" 2>/dev/null)" ]; then
    echo "  :: staging desktop overlay"
    cp -a --no-preserve=ownership "$FEATURE_DIR/overlay/." "$DESKTOP_PACKAGE_ROOT/"
    # /etc/environment is owned by the base system on many Debian installs.
    # The desktop package merges cursor defaults into it from postinst instead
    # of trying to own the file directly.
    rm -f "$DESKTOP_PACKAGE_ROOT/etc/environment"
fi

# ---------------------------------------------------------------------------
# 0b. Disk-space preflight.
#
# The cargo build needs ~15 GB and the installed desktop adds ~16 GB into the
# package root, all on the filesystem holding $ROOTFS. If that fills mid-build
# the failure surfaces an hour later as a cryptic copy error — and on WSL2 a
# full dynamic VHDX returns "Input/output error" (EIO) rather than ENOSPC, so
# `install` dies with "error copying ...: Input/output error" at the final
# binary. Fail fast and loudly here instead.
# ---------------------------------------------------------------------------
DESKTOP_MIN_FREE_GB="${DESKTOP_MIN_FREE_GB:-30}"
avail_kb="$(df -Pk "$ROOTFS" | awk 'NR==2 {print $4}')"
avail_gb=$(( avail_kb / 1024 / 1024 ))
if [ "$avail_gb" -lt "$DESKTOP_MIN_FREE_GB" ]; then
    cat >&2 <<EOF
  error: only ${avail_gb} GB free on the filesystem holding $ROOTFS
         the desktop build needs ~${DESKTOP_MIN_FREE_GB} GB (cargo cache + ~16 GB install root).
         Free up space and re-run. On WSL2, a full virtual disk shows up as
         "Input/output error" mid-copy — expand or compact the WSL VHDX, or
         clear space on the host drive backing it. Override the threshold with
         DESKTOP_MIN_FREE_GB if you know what you are doing.
EOF
    exit 1
fi
echo "  :: disk preflight ok (${avail_gb} GB free, need ~${DESKTOP_MIN_FREE_GB} GB)"

# ---------------------------------------------------------------------------
# 1. Validate source tree.
# ---------------------------------------------------------------------------

if [ ! -d "$DESKTOP_SRC" ] || [ ! -f "$DESKTOP_SRC/justfile" ]; then
    cat >&2 <<EOF
  error: desktop source not found at $DESKTOP_SRC
  Expected an in-tree vendored copy with a top-level justfile.
  Either:
    1. Run from a checked-out claw-os tree (desktop/ should exist).
    2. Set DESKTOP_SRC=/path/to/source-tree and re-run.
    3. Set DESKTOP_SKIP=1 to install runtime deps + overlay only (no DE binaries).
EOF
    exit 1
fi

echo "  :: validating desktop source tree at $DESKTOP_SRC"
missing=0
for sub in comp session panel launcher settings greeter toolkit; do
    [ -e "$DESKTOP_SRC/$sub" ] || { echo "    missing: $sub"; missing=1; }
done
[ "$missing" = "0" ] || {
    echo "  error: desktop source tree is incomplete"
    exit 1
}

# ---------------------------------------------------------------------------
# 1.5. Materialize Git LFS assets.
#
# Several desktop assets are tracked with Git LFS (see desktop/**/.gitattributes
# `filter=lfs`): the cosmic-initial-setup city database
# (initial-setup/res/cities.bitcode-*), wallpapers, the greeter background,
# app icons and fonts. A clone made on a machine without git-lfs leaves these
# as ~130-byte *pointer stubs* on disk. The build then happily compiles those
# stubs into the binaries via include_bytes!/copies them into the image — so
# the city search returns nothing, wallpapers are blank, etc. — and nothing
# fails loudly. Detect that here and pull the real objects before building.
# ---------------------------------------------------------------------------
lfs_is_pointer() {
    # Git LFS pointer files begin with "version https://git-lfs.github.com/...".
    head -c 64 "$1" 2>/dev/null | grep -q '^version https://git-lfs'
}

# Canary: the city database is always present and is the asset whose breakage
# is hardest to spot (silent empty timezone list). If it is real, every other
# LFS object pulled in the same clone is real too.
LFS_CANARY="$DESKTOP_SRC/initial-setup/res/cities.bitcode-v0-6"
if [ -f "$LFS_CANARY" ] && lfs_is_pointer "$LFS_CANARY"; then
    echo "  :: Git LFS assets are unfetched pointer stubs — materializing"

    if [ ! -d "$PROJECT_DIR/.git" ]; then
        cat >&2 <<EOF
  error: $LFS_CANARY is a Git LFS pointer but $PROJECT_DIR is not a git
         checkout, so 'git lfs pull' cannot fetch the real data. Re-clone
         with git-lfs installed, or provide a tree with materialized assets.
EOF
        exit 1
    fi

    if ! git lfs version >/dev/null 2>&1; then
        echo "  :: installing git-lfs"
        apt-get update
        apt-get install -y git-lfs
    fi

    # 'git lfs pull' must reach the remote, which needs the user's credentials:
    # an SSH key + known_hosts for git@github.com remotes, or a credential
    # helper / gh token for https remotes. The build runs as root (via sudo),
    # and root has neither — so pulling as root fails regardless of how the repo
    # was cloned ("ssh_askpass ... Host key verification failed" for SSH,
    # auth prompts for HTTPS). Run the LFS commands as the repo's OWNER instead,
    # so whatever transport + auth the user already set up just works for both.
    repo_owner="$(stat -c '%U' "$PROJECT_DIR")"
    run_as_owner() {
        if [ "$(id -un)" = "$repo_owner" ]; then
            "$@"
        elif command -v runuser >/dev/null 2>&1; then
            runuser -u "$repo_owner" -- "$@"
        else
            sudo -H -u "$repo_owner" -- "$@"
        fi
    }

    # Mark the tree safe for both identities (root and the owner) so neither
    # git invocation refuses with "dubious ownership".
    git config --global --add safe.directory "$PROJECT_DIR" 2>/dev/null || true
    run_as_owner git config --global --add safe.directory "$PROJECT_DIR" 2>/dev/null || true

    run_as_owner git -C "$PROJECT_DIR" lfs install --local
    run_as_owner git -C "$PROJECT_DIR" lfs pull

    if lfs_is_pointer "$LFS_CANARY"; then
        echo "  error: 'git lfs pull' did not materialize $LFS_CANARY" >&2
        echo "         (still a pointer stub). Check network / LFS quota." >&2
        exit 1
    fi
    echo "  :: Git LFS assets materialized"
fi

# ---------------------------------------------------------------------------
# 2. Build the desktop inside the chroot so binaries link against rootfs
#    glibc, not the host's.
#
#    Several desktop/* crates have `path = "../../crates/<x>"` dependencies
#    pointing at the top-level repo `crates/` directory (claw-bridge,
#    cos-mcp-serve, …). Bind-mount that too so the relative path resolves
#    inside the chroot (../../crates from /build/desktop-src/<x> →
#    /build/crates).
#
#    Likewise desktop/{term,edit,files,launcher}/Cargo.toml depend on
#    `path = "../../cos-runtime/rust"` for the internal SDK that wraps
#    every `cos app <id> <verb>` call (audit + caps + snapshot). Without
#    this mount cargo cannot resolve cos-runtime inside the chroot and
#    the desktop build fails before producing a single binary.
#
#    And cos-runtime itself pulls in `claw-os-sdk = { path =
#    "../../claw-os-sdk/rust" }` (the public app-developer SDK that
#    cos-runtime layers audit/caps on top of). Bind-mount that one too
#    or cargo dies in the desktop crates with "failed to read
#    /build/claw-os-sdk/rust/Cargo.toml".
# ---------------------------------------------------------------------------
CHROOT_SRC="$ROOTFS/build/desktop-src"
CHROOT_CRATES="$ROOTFS/build/crates"
CHROOT_RUNTIME="$ROOTFS/build/cos-runtime"
CHROOT_SDK="$ROOTFS/build/claw-os-sdk"
PROJECT_CRATES="$PROJECT_DIR/crates"
PROJECT_RUNTIME="$PROJECT_DIR/cos-runtime"
PROJECT_SDK="$PROJECT_DIR/claw-os-sdk"
mkdir -p "$CHROOT_SRC"
if ! mountpoint -q "$CHROOT_SRC"; then
    mount --bind "$DESKTOP_SRC" "$CHROOT_SRC"
fi
if [ -d "$PROJECT_CRATES" ]; then
    mkdir -p "$CHROOT_CRATES"
    if ! mountpoint -q "$CHROOT_CRATES"; then
        mount --bind "$PROJECT_CRATES" "$CHROOT_CRATES"
    fi
fi
if [ -d "$PROJECT_RUNTIME" ]; then
    mkdir -p "$CHROOT_RUNTIME"
    if ! mountpoint -q "$CHROOT_RUNTIME"; then
        mount --bind "$PROJECT_RUNTIME" "$CHROOT_RUNTIME"
    fi
fi
if [ -d "$PROJECT_SDK" ]; then
    mkdir -p "$CHROOT_SDK"
    if ! mountpoint -q "$CHROOT_SDK"; then
        mount --bind "$PROJECT_SDK" "$CHROOT_SDK"
    fi
fi

cleanup() {
    umount "$CHROOT_SDK" 2>/dev/null || true
    rmdir "$CHROOT_SDK" 2>/dev/null || true
    umount "$CHROOT_RUNTIME" 2>/dev/null || true
    rmdir "$CHROOT_RUNTIME" 2>/dev/null || true
    umount "$CHROOT_CRATES" 2>/dev/null || true
    rmdir "$CHROOT_CRATES" 2>/dev/null || true
    umount "$CHROOT_SRC" 2>/dev/null || true
    rmdir "$CHROOT_SRC" 2>/dev/null || true
    rmdir "$ROOTFS/build" 2>/dev/null || true
}
trap cleanup EXIT

# Rust toolchain inside the chroot. `rustup` package is in trixie; we use
# the minimal stable profile to keep image size down.
echo "  :: ensuring rustup toolchain in chroot"
chroot "$ROOTFS" bash -c '
    set -e
    # rustup show active-toolchain exits 0 even when nothing is configured,
    # so check `rustup default` instead. Output is empty when no default set.
    if [ -z "$(rustup default 2>/dev/null)" ]; then
        rustup toolchain install stable --profile minimal
        rustup default stable
    fi
    export PATH="/root/.cargo/bin:$PATH"
    command -v just >/dev/null || cargo install --quiet just
'

echo "  :: building desktop (cold tree: 30–60 minutes)"
# Several desktop crates (greeter, player) use `vergen` in their build.rs to
# embed VERGEN_GIT_SHA / VERGEN_GIT_COMMIT_DATE at compile time. The chroot
# has no .git so vergen fails. Pre-compute on the host and pass through.
VERGEN_GIT_SHA="$(git_readonly -C "$PROJECT_DIR" rev-parse HEAD 2>/dev/null || echo unknown)"
VERGEN_GIT_COMMIT_DATE="$(git_readonly -C "$PROJECT_DIR" log -1 --format=%cs HEAD 2>/dev/null || date -u +%Y-%m-%d)"
chroot "$ROOTFS" env \
    VERGEN_GIT_SHA="$VERGEN_GIT_SHA" \
    VERGEN_GIT_COMMIT_DATE="$VERGEN_GIT_COMMIT_DATE" \
    DESKTOP_PACKAGE_ROOT="$DESKTOP_PACKAGE_ROOT_CHROOT" \
    bash -c '
    set -e
    export CARGO_HOME=/root/.cargo
    export PATH="$CARGO_HOME/bin:$PATH"
    # One shared target dir for all ~24 desktop crates. Each crate opts out
    # of the repo-root workspace and so defaults to its own ./target, which
    # recompiles and re-stores the whole shared dependency tree (libcosmic,
    # iced, wgpu, …) once per crate — ~2 GB each, ~45 GB for the full set,
    # which does not fit alongside the ~16 GB install root. Sharing one dir
    # builds those deps once. Every crate honours this: the justfiles read
    # env("CARGO_TARGET_DIR") and the Makefiles use CARGO_TARGET_DIR ?=,
    # for both the build and the install path they copy the binary from.
    #
    # It lives under the bind-mounted source tree, so it stays on the host
    # as an incremental cache between runs and never lands in the image.
    export CARGO_TARGET_DIR=/build/desktop-src/target
    cd /build/desktop-src
    just build
    # NB: pass rootdir and prefix as POSITIONAL args. `just install rootdir=""`
    # would set rootdir to the literal string "rootdir=" (the entire token is
    # the value of positional param 1), producing nonsense install paths like
    # `/build/desktop-src/rootdir=/prefix=/usr/bin/cosmic-greeter`. The
    # cosmic-* binaries then never reach /usr/bin and the resulting image has
    # no working desktop. See desktop/justfile recipe `install rootdir="" prefix="/usr/local"`.
    just install "$DESKTOP_PACKAGE_ROOT" /usr

    # Install the canonical full COSMIC session entry explicitly at the
    # package boundary. `desktop/comp` also carries a same-named bare-session
    # file for standalone compositor installs; relying on recursive install
    # order can therefore leave display managers pointing at cosmic-service
    # instead of the full start-cosmic session.
    install -Dm0644 session/data/cosmic.desktop \
        "$DESKTOP_PACKAGE_ROOT/usr/share/wayland-sessions/cosmic.desktop"
    test -x "$DESKTOP_PACKAGE_ROOT/usr/bin/start-cosmic"
    grep -qx "Exec=/usr/bin/start-cosmic" \
        "$DESKTOP_PACKAGE_ROOT/usr/share/wayland-sessions/cosmic.desktop"

    # ----------------------------------------------------------------------
    # ClawOS Agent UI + bridge — separate workspace (no justfile) under
    # desktop/agent/. com.clawos.Agent.desktop expects /usr/local/bin/cos-agent-ui
    # via `cos app agent open`; the cos-agent-bridge.service user unit
    # invokes /usr/local/bin/cos-agent-bridge. These binaries and the secure
    # SDK launcher helper must be
    # produced here or the desktop agent app fails to launch with
    #   {"error":"cos-agent-ui is not installed"}.
    # ----------------------------------------------------------------------
    if [ ! -f /build/desktop-src/agent/Cargo.toml ]; then
        echo "error: required desktop Agent workspace is missing" >&2
        exit 1
    fi
    if [ ! -f /build/cos-runtime/rust/Cargo.toml ]; then
        echo "error: required cos-runtime helper source is missing" >&2
        exit 1
    fi
    echo "  :: building cos-agent-ui + cos-agent-bridge + cos-ask-claw-launcher"
    cd /build/desktop-src/agent
    cargo build --release --workspace
    cargo build --release --manifest-path /build/cos-runtime/rust/Cargo.toml \
        --bin cos-ask-claw-launcher
    # Honour CARGO_TARGET_DIR (exported above) — cargo writes there, so
    # the binaries are not under ./target when it is set.
    agent_target="${CARGO_TARGET_DIR:-target}"
    install -Dm0755 "$agent_target/release/cos-agent-ui"     "$DESKTOP_PACKAGE_ROOT/usr/local/bin/cos-agent-ui"
    install -Dm0755 "$agent_target/release/cos-agent-bridge" "$DESKTOP_PACKAGE_ROOT/usr/local/bin/cos-agent-bridge"
    install -Dm0755 "$agent_target/release/cos-ask-claw-launcher" "$DESKTOP_PACKAGE_ROOT/usr/local/bin/cos-ask-claw-launcher"
'

# ---------------------------------------------------------------------------
# 2b. Assert that critical data files landed in the package staging root.
#
# These are silently-required by the desktop and have, in the past, gone
# missing without breaking the build — leading to bugs like "Panel/Dock
# settings page shows only Reset to Default" (missing schema files under
# /usr/share/cosmic/com.clawos.Panel.*/v1/) or "wallpaper page has no preview"
# (missing /usr/share/backgrounds/cosmic/claw-default.jpg).
#
# If any of these are missing here, the package would produce a broken image —
# fail loudly instead of shipping it.
# ---------------------------------------------------------------------------
echo "  :: verifying critical desktop data files"
required_files=(
    # Wallpaper bitmap + cosmic-bg default entry
    "$DESKTOP_PACKAGE_ROOT/usr/share/backgrounds/cosmic/claw-default.jpg"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Background/v1/all"
    # Panel + Dock default schemas — the "name" file is the canary;
    # if it's missing the settings page collapses to "Reset to Default".
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Panel.Panel/v1/name"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Panel.Panel/v1/padding_overlap"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Panel.Dock/v1/name"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Panel.Dock/v1/padding_overlap"
    # Theme + comp defaults
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Theme.Dark/v1/name"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Theme.Light/v1/name"
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Theme.Mode/v1/is_dark"
    # cosmic-comp appearance — enables window shadows + corner clipping by
    # default. Without this file cosmic-comp falls back to the AppearanceConfig
    # struct Default (now also shadow_tiled_windows=true, but seed wins).
    "$DESKTOP_PACKAGE_ROOT/usr/share/cosmic/com.clawos.Comp/v1/appearance_settings"
    # First-party application icon family. Scalable hicolor assets avoid
    # fixed-size theme overrides and keep every shell surface consistent.
    "$DESKTOP_PACKAGE_ROOT/usr/share/icons/hicolor/scalable/apps/com.clawos.Files.svg"
    "$DESKTOP_PACKAGE_ROOT/usr/share/icons/hicolor/scalable/apps/com.clawos.Term.svg"
    "$DESKTOP_PACKAGE_ROOT/usr/share/icons/hicolor/scalable/apps/com.clawos.Edit.svg"
    "$DESKTOP_PACKAGE_ROOT/usr/share/icons/hicolor/scalable/apps/com.clawos.Settings.svg"
    "$DESKTOP_PACKAGE_ROOT/usr/share/icons/hicolor/scalable/apps/com.clawos.Store.svg"
    "$DESKTOP_PACKAGE_ROOT/usr/share/icons/hicolor/scalable/apps/com.clawos.Player.svg"
    "$DESKTOP_PACKAGE_ROOT/usr/share/icons/hicolor/scalable/apps/com.clawos.Launcher.svg"
    "$DESKTOP_PACKAGE_ROOT/usr/share/icons/hicolor/scalable/apps/com.clawos.AppLibrary.svg"
    # Icon theme — Tela is installed via just install
    # icons-tela-pkg/install in desktop/justfile. If it's missing
    # the toolkit light default falls back to hicolor and loses the
    # intended system glyph family.
    "$DESKTOP_PACKAGE_ROOT/usr/share/icons/Tela-black-light/index.theme"
)
missing=0
for f in "${required_files[@]}"; do
    if [ ! -e "$f" ]; then
        echo "    missing: ${f#$DESKTOP_PACKAGE_ROOT}"
        missing=1
    fi
done
if [ "$missing" = "1" ]; then
    echo "  error: critical desktop data files are missing — the resulting" >&2
    echo "         image would have broken Settings + Wallpaper pages."     >&2
    echo "         Check desktop/justfile install recipes and confirm"      >&2
    echo "         the source tree under $DESKTOP_SRC is intact."           >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 3. Stage login-chain wiring into claw-os-desktop.deb.
#
# `just install` puts the binaries / .desktop / sysusers / tmpfiles in
# place, but the upstream Debian packaging (which we are NOT using) is
# responsible for systemd .service files, the greetd config, and the PAM stack.
# We stage those files into the deb and let postinst handle users, locale-gen,
# systemctl enable, sysusers/tmpfiles, and plymouth activation.
# ---------------------------------------------------------------------------
GREETER_DEB="$DESKTOP_SRC/greeter/debian"

echo "  :: installing greeter systemd units, PAM, greetd config"
install -Dm0644 "$GREETER_DEB/cosmic-greeter.service" \
    "$DESKTOP_PACKAGE_ROOT/lib/systemd/system/cosmic-greeter.service"
install -Dm0644 "$GREETER_DEB/cosmic-greeter-daemon.service" \
    "$DESKTOP_PACKAGE_ROOT/lib/systemd/system/cosmic-greeter-daemon.service"
install -Dm0644 "$GREETER_DEB/cosmic-greeter.pam" \
    "$DESKTOP_PACKAGE_ROOT/etc/pam.d/cosmic-greeter"
install -Dm0644 "$DESKTOP_SRC/greeter/cosmic-greeter.toml" \
    "$DESKTOP_PACKAGE_ROOT/etc/greetd/cosmic-greeter.toml"

# ---------------------------------------------------------------------------
# First-boot wizard wiring.
#
# Fresh images have no human user with a usable password, so cosmic-greeter
# has nothing to log into. Pop!_OS solves this with an installer ISO; we
# don't ship one, so we run cosmic-initial-setup as a first-boot wizard
# inside greetd's [initial_session]:
#
#   1. Create system user `cosmic-initial-setup` (uid <1000, hidden from
#      the cosmic-greeter user list by UserFilter).
#   2. Append [initial_session] to /etc/greetd/cosmic-greeter.toml
#      pointing at /usr/lib/cos/firstboot-session — a wrapper that either
#      execs cosmic-session (no human user yet) or exits 0 so greetd
#      falls through to [default_session] (wizard already ran).
#
# The firstboot-session wrapper kiosk-launches the wizard directly
# (`cosmic-comp cosmic-initial-setup`). When the wizard's Finish handler runs
# `loginctl terminate-user cosmic-initial-setup`, the session dies and greetd
# advances to cosmic-greeter for normal login as the user the wizard created.
# ---------------------------------------------------------------------------
echo "  :: staging first-boot wizard greetd config"
if ! grep -q '^\[initial_session\]' "$DESKTOP_PACKAGE_ROOT/etc/greetd/cosmic-greeter.toml"; then
    cat >> "$DESKTOP_PACKAGE_ROOT/etc/greetd/cosmic-greeter.toml" <<'EOF'

[initial_session]
command = "/usr/lib/cos/firstboot-session"
user = "cosmic-initial-setup"
EOF
fi

# Drive the wizard ONLY via the firstboot-session kiosk above. The upstream
# autostart entry (/etc/xdg/autostart/com.clawos.InitialSetup.desktop) would
# also relaunch the wizard inside every normal cosmic-session — so the first
# real login lands on the desktop and the wizard pops on top of it. Remove it
# so logging in goes straight to a working desktop.
rm -f "$DESKTOP_PACKAGE_ROOT/etc/xdg/autostart/com.clawos.InitialSetup.desktop"

# Create the cosmic-greeter system user + its runtime/state dirs from the
# sysusers.d / tmpfiles.d that `just install` already dropped. Package postinst
# runs systemd-sysusers/systemd-tmpfiles after the files are installed.

# Upstream cosmic-greeter.service has its [Install] section commented out
# (the deb postinst manages display-manager.service symlinking via debconf).
# We are not running dpkg, so wire the systemd targets explicitly.
echo "  :: enabling display-manager + supporting services"
mkdir -p "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/graphical.target.wants"
mkdir -p "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/multi-user.target.wants"

ln -sf /lib/systemd/system/cosmic-greeter.service \
    "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/graphical.target.wants/cosmic-greeter.service"
ln -sf /lib/systemd/system/cosmic-greeter.service \
    "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/display-manager.service"
ln -sf /lib/systemd/system/cosmic-greeter-daemon.service \
    "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/multi-user.target.wants/cosmic-greeter-daemon.service"

# Boot to graphical.target by default.
ln -sf /lib/systemd/system/graphical.target \
    "$DESKTOP_PACKAGE_ROOT/etc/systemd/system/default.target"

# Per-user services (PipeWire, WirePlumber, xdg-desktop-portal). These ship
# with default.target.wants symlinks from their deb packages, but in case
# they ever stop doing so, force-enable them here in /etc/systemd/user/.
mkdir -p "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/sockets.target.wants"
mkdir -p "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/default.target.wants"
for unit in pipewire.socket pipewire-pulse.socket; do
    [ -e "$ROOTFS/usr/lib/systemd/user/$unit" ] && \
        ln -sf "/usr/lib/systemd/user/$unit" \
            "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/sockets.target.wants/$unit"
done
for unit in pipewire.service wireplumber.service; do
    [ -e "$ROOTFS/usr/lib/systemd/user/$unit" ] && \
        ln -sf "/usr/lib/systemd/user/$unit" \
            "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/default.target.wants/$unit"
done

# cos-agent-bridge.service is shipped in the desktop package overlay. It
# declares WantedBy=graphical-session.target
# but `systemctl --user enable` only runs in the user's session, which
# means a fresh user (no prior login) never gets the symlink. Wire it
# globally so the bridge starts as soon as cosmic-session reaches
# graphical-session.target.
mkdir -p "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/graphical-session.target.wants"
[ -e "$DESKTOP_PACKAGE_ROOT/usr/lib/systemd/user/cos-agent-bridge.service" ] && \
    ln -sf "/usr/lib/systemd/user/cos-agent-bridge.service" \
        "$DESKTOP_PACKAGE_ROOT/etc/systemd/user/graphical-session.target.wants/cos-agent-bridge.service"

# ---------------------------------------------------------------------------
# 4. Build and install claw-os-desktop.deb.
# ---------------------------------------------------------------------------
echo "  :: building claw-os-desktop.deb"
"$PROJECT_DIR/packaging/deb/build-desktop-deb.sh" "$DESKTOP_PACKAGE_ROOT"

DEBS_DIR="$PROJECT_DIR/build/debs"
DESKTOP_DEB="$DEBS_DIR/claw-os-desktop_${COS_VERSION}_${DEB_ARCH:-amd64}.deb"
if [ ! -f "$DESKTOP_DEB" ]; then
    echo "  error: expected desktop package missing: $DESKTOP_DEB" >&2
    exit 1
fi
if [ "${DESKTOP_PACKAGE_ONLY:-0}" = "1" ]; then
    echo "  :: DESKTOP_PACKAGE_ONLY=1 -- package built without installing into rootfs"
    rm -rf "$DESKTOP_PACKAGE_ROOT"
    exit 0
fi
echo "  :: installing $(basename "$DESKTOP_DEB")"

mkdir -p "$ROOTFS/var/cache/cos-debs"
cp "$DESKTOP_DEB" "$ROOTFS/var/cache/cos-debs/"
# The desktop deb ships some conffiles (e.g. /etc/apt/apt.conf.d/20auto-upgrades)
# that already exist on the rootfs from the base overlay. dpkg would normally
# stop at an interactive conffile prompt, but stdin is not a terminal inside the
# chroot, so the prompt hits EOF and aborts. Force the non-interactive default
# (keep the existing file: confdef + confold) and set the noninteractive frontend.
DEBIAN_FRONTEND=noninteractive chroot "$ROOTFS" apt-get install -y \
    --no-install-recommends \
    -o Dpkg::Options::=--force-confdef \
    -o Dpkg::Options::=--force-confold \
    "/var/cache/cos-debs/$(basename "$DESKTOP_DEB")"
chroot "$ROOTFS" apt-get clean
rm -rf "$ROOTFS/var/lib/apt/lists"/*
rm -rf "$DESKTOP_PACKAGE_ROOT"

# ---------------------------------------------------------------------------
# 5. Verify package install side effects.
# ---------------------------------------------------------------------------
echo "  :: verifying claw-os-desktop install"
if ! chroot "$ROOTFS" dpkg-query -W -f='${Status}' claw-os-desktop 2>/dev/null \
    | grep -qx 'install ok installed'; then
    echo "  error: claw-os-desktop package is not installed" >&2
    exit 1
fi

installed_required_files=(
    "$ROOTFS/usr/share/backgrounds/cosmic/claw-default.jpg"
    "$ROOTFS/usr/share/cosmic/com.clawos.Panel.Panel/v1/name"
    "$ROOTFS/usr/share/cosmic/com.clawos.Panel.Dock/v1/name"
    "$ROOTFS/usr/share/cosmic/com.clawos.Theme.Dark/v1/name"
    "$ROOTFS/usr/share/icons/hicolor/scalable/apps/com.clawos.Files.svg"
    "$ROOTFS/usr/share/icons/hicolor/scalable/apps/com.clawos.Term.svg"
    "$ROOTFS/usr/share/icons/hicolor/scalable/apps/com.clawos.Edit.svg"
    "$ROOTFS/usr/share/icons/hicolor/scalable/apps/com.clawos.Settings.svg"
    "$ROOTFS/usr/share/icons/hicolor/scalable/apps/com.clawos.Store.svg"
    "$ROOTFS/usr/share/icons/hicolor/scalable/apps/com.clawos.Player.svg"
    "$ROOTFS/usr/share/icons/hicolor/scalable/apps/com.clawos.Launcher.svg"
    "$ROOTFS/usr/share/icons/hicolor/scalable/apps/com.clawos.AppLibrary.svg"
    "$ROOTFS/usr/share/icons/hicolor/scalable/apps/clawos-agent.svg"
    "$ROOTFS/usr/share/applications/com.clawos.Agent.desktop"
    "$ROOTFS/usr/lib/systemd/user/cos-agent-bridge.service"
    "$ROOTFS/usr/local/bin/cos-agent-ui"
    "$ROOTFS/usr/local/bin/cos-agent-bridge"
    "$ROOTFS/usr/local/bin/cos-ask-claw-launcher"
    "$ROOTFS/usr/share/icons/Tela-black-light/index.theme"
    "$ROOTFS/etc/greetd/cosmic-greeter.toml"
    "$ROOTFS/lib/systemd/system/cosmic-greeter.service"
)
install_missing=0
for f in "${installed_required_files[@]}"; do
    if [ ! -e "$f" ]; then
        echo "    missing after package install: ${f#$ROOTFS}"
        install_missing=1
    fi
done
if [ "$install_missing" = "1" ]; then
    echo "  error: claw-os-desktop installed with missing desktop files" >&2
    exit 1
fi

while IFS= read -r app_id; do
    [ -n "$app_id" ] || continue
    if [ ! -f "$ROOTFS/usr/lib/cos/apps/$app_id/app.json" ]; then
        echo "  error: desktop app missing after package install: $app_id" >&2
        exit 1
    fi
done < "$PROJECT_DIR/packaging/deb/claw-os-desktop/apps.list"

locale_missing=0
for want in en_US.utf8 zh_CN.utf8 zh_TW.utf8 ja_JP.utf8; do
    if ! chroot "$ROOTFS" locale -a 2>/dev/null | grep -qx "$want"; then
        echo "    missing locale: $want"
        locale_missing=1
    fi
done
if [ "$locale_missing" = "1" ]; then
    echo "  error: UTF-8 locales failed to generate — cosmic-initial-setup" >&2
    echo "         Language page would be empty. Check claw-os-desktop postinst." >&2
    exit 1
fi

echo "  :: desktop installed via claw-os-desktop.deb; default target = graphical.target"
echo "  :: greeter:  /etc/systemd/system/display-manager.service -> cosmic-greeter.service"
echo "  :: greetd cfg: /etc/greetd/cosmic-greeter.toml"
