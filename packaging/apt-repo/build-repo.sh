#!/usr/bin/env bash
# packaging/apt-repo/build-repo.sh — assemble an apt repository at
# build/apt-repo/ from the .debs in build/debs/.
#
# Layout produced (Debian "flat-and-pool" style):
#
#   build/apt-repo/
#   ├── dists/trixie/
#   │   ├── InRelease           (signed Release, omitted if no GPG key)
#   │   ├── Release             (always)
#   │   ├── Release.gpg         (detached signature, omitted if no GPG key)
#   │   └── main/
#   │       ├── binary-amd64/Packages{,.gz}    (if amd64 .debs present)
#   │       ├── binary-arm64/Packages{,.gz}    (if arm64 .debs present)
#   │       └── binary-all/Packages{,.gz}      (always — Architecture: all)
#   └── pool/main/c/claw-os-base/claw-os-base_<v>_<arch>.deb
#       pool/main/c/claw-os-browser/claw-os-browser_<v>_<arch>.deb
#       pool/main/c/claw-os-systemd/claw-os-systemd_<v>_all.deb
#       pool/main/c/claw-os-desktop/claw-os-desktop_<v>_<arch>.deb
#
# Dual-arch: the script auto-discovers every Architecture: in build/debs/
# and emits one binary-<arch>/ tree per architecture, so an admin can run
# build-debs.sh twice (once on an amd64 host, once on an arm64 host)
# into the same build/debs/ directory and produce a single multi-arch repo.
#
# The repo is unsigned by default. Set GPG_KEY_ID to enable signing.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

DEBS_DIR="$PROJECT_DIR/build/debs"
REPO_DIR="$PROJECT_DIR/build/apt-repo"
BRAND_ASSETS_DIR="$PROJECT_DIR/assets/brand"
SUITE="${SUITE:-trixie}"
COMPONENT="main"
GPG_KEY_ID="${GPG_KEY_ID:-}"

if [ ! -d "$DEBS_DIR" ] || [ -z "$(ls "$DEBS_DIR"/*.deb 2>/dev/null)" ]; then
    echo "error: no .debs in $DEBS_DIR — run packaging/deb/build-debs.sh first" >&2
    exit 1
fi

if ! command -v apt-ftparchive >/dev/null 2>&1; then
    echo "error: apt-ftparchive not found. Install it with: apt-get install apt-utils" >&2
    exit 1
fi

# Discover every Architecture: in the .deb filenames. Conventional Debian
# filename is `<pkg>_<version>_<arch>.deb`. We extract the final field.
declare -a binary_arches=()
arch_seen=""
for deb in "$DEBS_DIR"/*.deb; do
    name="$(basename "$deb")"
    # claw-os-base_0.1.0_amd64.deb -> amd64
    deb_arch="${name##*_}"
    deb_arch="${deb_arch%.deb}"
    # Architecture: all packages are surfaced under every binary-<arch>
    # tree by apt's resolver, so we only iterate over real arches here.
    [ "$deb_arch" = "all" ] && continue
    case " $arch_seen " in
        *" $deb_arch "*) ;;
        *) binary_arches+=("$deb_arch"); arch_seen="$arch_seen $deb_arch" ;;
    esac
done

if [ ${#binary_arches[@]} -eq 0 ]; then
    echo "error: no architecture-specific .debs found in $DEBS_DIR" >&2
    exit 1
fi

echo ":: building apt repo at $REPO_DIR"
echo ":: arches: ${binary_arches[*]}"

rm -rf "$REPO_DIR"
for a in "${binary_arches[@]}"; do
    mkdir -p "$REPO_DIR/dists/$SUITE/$COMPONENT/binary-$a"
done
mkdir -p "$REPO_DIR/dists/$SUITE/$COMPONENT/binary-all"
mkdir -p "$REPO_DIR/assets/brand"
cp "$BRAND_ASSETS_DIR/clawos-wordmark.png" \
   "$BRAND_ASSETS_DIR/clawos-favicon-64.png" \
   "$REPO_DIR/assets/brand/"

# Move each .deb into pool/main/c/<package-name>/.
for deb in "$DEBS_DIR"/*.deb; do
    name="$(basename "$deb")"
    # claw-os-base_0.1.0_amd64.deb -> claw-os-base
    pkg="${name%%_*}"
    pool_dir="$REPO_DIR/pool/$COMPONENT/c/$pkg"
    mkdir -p "$pool_dir"
    cp "$deb" "$pool_dir/"
    echo "  :: pool/$COMPONENT/c/$pkg/$name"
done

# Generate Packages files. apt-ftparchive packages walks the pool and
# extracts the Architecture field from each .deb's control. The same
# pool feeds every binary-<arch>/ index; apt's client filters by arch
# at install time.
cd "$REPO_DIR"
echo ":: generating Packages indexes"
for a in "${binary_arches[@]}"; do
    apt-ftparchive --arch "$a" packages "pool/$COMPONENT" \
        > "dists/$SUITE/$COMPONENT/binary-$a/Packages"
    gzip -fk9 "dists/$SUITE/$COMPONENT/binary-$a/Packages"
done

# Architecture: all packages need an explicit binary-all index. We pass
# `--arch all` so apt-ftparchive only picks up Architecture: all .debs.
apt-ftparchive --arch all packages "pool/$COMPONENT" \
    > "dists/$SUITE/$COMPONENT/binary-all/Packages"
gzip -fk9 "dists/$SUITE/$COMPONENT/binary-all/Packages"

# Generate the Release file. The Architectures: list determines which
# binary-<arch>/ trees apt will fetch.
echo ":: generating Release"
arch_list="${binary_arches[*]} all"
cat > "$REPO_DIR/apt-ftparchive-release.conf" <<EOF
APT::FTPArchive::Release::Origin "Claw OS";
APT::FTPArchive::Release::Label "Claw OS";
APT::FTPArchive::Release::Suite "$SUITE";
APT::FTPArchive::Release::Codename "$SUITE";
APT::FTPArchive::Release::Architectures "$arch_list";
APT::FTPArchive::Release::Components "$COMPONENT";
APT::FTPArchive::Release::Description "Claw OS apt repository";
EOF

apt-ftparchive -c="$REPO_DIR/apt-ftparchive-release.conf" \
    release "dists/$SUITE" > "dists/$SUITE/Release"

rm -f "$REPO_DIR/apt-ftparchive-release.conf"

# Sign the repo if a key is configured.
if [ -n "$GPG_KEY_ID" ]; then
    echo ":: signing with GPG key $GPG_KEY_ID"
    gpg --batch --yes --default-key "$GPG_KEY_ID" --detach-sign \
        --armor -o "dists/$SUITE/Release.gpg" "dists/$SUITE/Release"
    gpg --batch --yes --default-key "$GPG_KEY_ID" --clearsign \
        -o "dists/$SUITE/InRelease" "dists/$SUITE/Release"
    # Export the public key so users can fetch + trust it.
    gpg --armor --export "$GPG_KEY_ID" > "$REPO_DIR/claw-os.gpg"
    echo "  :: signed; public key at $REPO_DIR/claw-os.gpg"
else
    echo "  :: GPG_KEY_ID not set — repo is unsigned (use [trusted=yes])"
fi

# GitHub Pages homepage. Keep this at the repo root so the APT paths remain
# stable: /dists/... and /pool/... are still served beside the marketing page.
{
    cat <<EOF
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light">
<meta name="description" content="Claw OS is The First Agent Native Operating System, a complete Linux system with structured control surfaces for AI agents.">
<title>Claw OS - The First Agent Native Operating System</title>
<link rel="icon" type="image/png" href="assets/brand/clawos-favicon-64.png">
<style>
:root {
  --bg: #fff;
  --bg-soft: #fafafa;
  --ink: #000;
  --muted: #666;
  --faint: #8a8a8a;
  --line: #eaeaea;
  --line-strong: #d8d8d8;
  --blue: #005cfe;
  --cyan: #00b7ff;
  --violet: #7c3aed;
  --green: #00a66a;
  --shadow: 0 24px 80px rgba(0, 0, 0, .08);
}

* { box-sizing: border-box; }
html { scroll-behavior: smooth; }
body {
  margin: 0;
  color: var(--ink);
  background:
    radial-gradient(circle at 50% -16%, rgba(0, 92, 254, .10), transparent 32rem),
    linear-gradient(#fff, #fafafa 68%, #fff);
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  -webkit-font-smoothing: antialiased;
}

a { color: inherit; text-decoration: none; }
code, pre { font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace; }
.page { position: relative; overflow: hidden; }
.container { width: min(1080px, calc(100% - 40px)); margin: 0 auto; }

.nav {
  position: sticky;
  top: 0;
  z-index: 20;
  border-bottom: 1px solid rgba(0, 0, 0, .06);
  background: rgba(255, 255, 255, .78);
  backdrop-filter: saturate(180%) blur(18px);
}
.nav-inner { height: 64px; display: flex; align-items: center; justify-content: space-between; gap: 24px; }
.brand { display: inline-flex; align-items: center; }
.brand-logo { display: block; width: 118px; height: auto; }
.nav-links { display: flex; align-items: center; gap: 22px; color: var(--muted); font-size: 14px; }
.nav-links a:hover { color: var(--ink); }
.nav-cta {
  display: inline-flex;
  align-items: center;
  height: 36px;
  padding: 0 14px;
  border: 1px solid #000;
  border-radius: 999px;
  color: #fff;
  background: #000;
  font-size: 14px;
  font-weight: 520;
}

.hero { padding: 72px 0 42px; text-align: center; }
.hero-logo { display: block; width: min(210px, 58vw); height: auto; margin: 0 auto 22px; }
h1 {
  max-width: 900px;
  margin: 26px auto 0;
  font-size: clamp(42px, 6vw, 72px);
  line-height: 1.04;
  letter-spacing: -.045em;
  font-weight: 720;
  text-wrap: balance;
}
h1 .line {
  display: block;
  white-space: nowrap;
}
.gradient-text {
  background: linear-gradient(90deg, #000 0%, #1f2937 34%, var(--blue) 72%, var(--violet) 100%);
  -webkit-background-clip: text;
  background-clip: text;
  color: transparent;
}
.actions { display: flex; justify-content: center; gap: 12px; margin-top: 30px; flex-wrap: wrap; }
.button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  height: 46px;
  padding: 0 20px;
  border-radius: 999px;
  border: 1px solid var(--line-strong);
  background: #fff;
  color: #111;
  font-weight: 560;
  box-shadow: 0 6px 24px rgba(0, 0, 0, .05);
}
.button.primary { border-color: #000; background: #000; color: #fff; }
.button:hover { transform: translateY(-1px); transition: transform .18s ease, border-color .18s ease; }

.product-shell {
  position: relative;
  margin: 0 auto;
  max-width: 760px;
  border: 1px solid #1f1f1f;
  border-radius: 20px;
  background: #0a0a0a;
  box-shadow: 0 22px 60px rgba(0, 0, 0, .13);
  overflow: hidden;
}
.product-topbar {
  height: 42px;
  border-bottom: 1px solid #1f1f1f;
  display: flex;
  align-items: center;
  padding: 0 16px;
  gap: 8px;
  background: #111;
}
.dot { width: 11px; height: 11px; border-radius: 999px; background: #ff5f57; }
.dot:nth-child(2) { background: #febc2e; }
.dot:nth-child(3) { background: #28c840; }
.terminal {
  display: flex;
  flex-direction: column;
  gap: 12px;
  text-align: left;
  padding: 24px;
  color: #f5f5f5;
  font-size: 14px;
  line-height: 1.6;
}
.prompt { color: var(--blue); font-weight: 700; }
.terminal .answer { color: #a3a3a3; }
.terminal-line {
  display: grid;
  grid-template-columns: 24px 1fr;
  gap: 10px;
  align-items: start;
}

.terminal-preview { padding-bottom: 56px; }
.section { padding: 56px 0; }
.section-head { margin-bottom: 24px; }
.kicker { color: var(--blue); font-size: 13px; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; }
h2 { margin: 8px 0 0; font-size: clamp(28px, 3.2vw, 40px); line-height: 1.08; letter-spacing: -.035em; }

.cards { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px; }
.feature-card { min-height: 128px; padding: 20px; border: 1px solid var(--line); border-radius: 16px; background: #fff; }
.feature-card h3 { margin: 0 0 8px; font-size: 19px; letter-spacing: -.02em; }
.feature-card p { margin: 0; color: var(--muted); line-height: 1.5; font-size: 15px; }

.install-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 18px; }
.install-card {
  border: 1px solid var(--line);
  border-radius: 24px;
  padding: 24px;
  background: #fff;
  box-shadow: 0 16px 60px rgba(0, 0, 0, .045);
}
.install-card h3 { margin: 0 0 12px; font-size: 24px; letter-spacing: -.035em; }
.install-card p { margin: 0 0 16px; color: var(--muted); line-height: 1.55; }
pre {
  margin: 0;
  padding: 18px;
  border: 1px solid var(--line);
  border-radius: 16px;
  overflow-x: auto;
  background: #0a0a0a;
  color: #f5f5f5;
  font-size: 13px;
  line-height: 1.6;
}
.footer { border-top: 1px solid var(--line); padding: 34px 0; color: var(--muted); font-size: 14px; }
.footer-inner { display: flex; justify-content: space-between; gap: 20px; flex-wrap: wrap; }

@media (max-width: 900px) {
  .nav-links { display: none; }
  .hero { padding-top: 62px; }
  h1 { font-size: clamp(42px, 13vw, 70px); letter-spacing: -.055em; }
  h1 .line { white-space: normal; }
  .install-grid { grid-template-columns: 1fr; }
  .cards { grid-template-columns: 1fr; }
}
</style>
</head>
<body>
<div class="page">
  <header class="nav">
    <div class="container nav-inner">
      <a class="brand" href="#">
        <img class="brand-logo" src="assets/brand/clawos-wordmark.png" alt="Claw OS">
      </a>
      <nav class="nav-links" aria-label="Primary">
        <a href="#features">Features</a>
        <a href="#install">Install</a>
        <a href="https://github.com/xiaoyu-work/claw-os">GitHub</a>
      </nav>
      <a class="nav-cta" href="#install">Get started</a>
    </div>
  </header>

  <main>
    <section class="hero">
      <div class="container">
        <img class="hero-logo" src="assets/brand/clawos-wordmark.png" alt="Claw OS">
        <h1><span class="line">The First Agent Native</span><span class="line gradient-text">Operating System</span></h1>
        <div class="actions">
          <a class="button primary" href="#install">Install Claw OS</a>
          <a class="button" href="https://github.com/xiaoyu-work/claw-os">View on GitHub</a>
        </div>
      </div>
    </section>

    <section class="container terminal-preview" aria-label="Terminal preview">
      <div class="product-shell">
        <div class="product-topbar">
          <span class="dot"></span><span class="dot"></span><span class="dot"></span>
        </div>
        <div class="terminal">
          <div class="terminal-line"><span class="prompt">$</span><span>cos agent ask "prepare this workspace"</span></div>
          <div class="terminal-line answer"><span></span><span>Plan ready. No files changed.</span></div>
          <div class="terminal-line"><span class="prompt">$</span><span>cos checkpoint diff</span></div>
          <div class="terminal-line answer"><span></span><span>No changes yet.</span></div>
        </div>
      </div>
    </section>

    <section id="features" class="section">
      <div class="container">
        <div class="section-head">
          <div>
            <div class="kicker">Core ideas</div>
            <h2>Built for agents to operate safely.</h2>
          </div>
        </div>
        <div class="cards">
          <article class="feature-card">
            <h3>Headless Linux</h3>
            <p>Run a complete Linux system without requiring a desktop session.</p>
          </article>
          <article class="feature-card">
            <h3>Structured control</h3>
            <p>Expose OS actions through predictable <code>cos</code> commands and JSON output.</p>
          </article>
          <article class="feature-card">
            <h3>Scoped execution</h3>
            <p>Gate risky actions with explicit capability checks and approvals.</p>
          </article>
          <article class="feature-card">
            <h3>Reversible work</h3>
            <p>Use checkpoints to inspect and roll back agent-made file changes.</p>
          </article>
        </div>
      </div>
    </section>

    <section id="install" class="section">
      <div class="container">
        <div class="section-head">
          <div>
            <div class="kicker">Install</div>
            <h2>Install Claw OS.</h2>
          </div>
          <p class="section-copy">Desktop, ISO, and VM images are experimental.</p>
        </div>
        <div class="install-grid">
          <article class="install-card">
            <h3>WSL</h3>
            <p>Import the latest rootfs on Windows.</p>
            <pre><code># Download the matching release asset, then:
wsl --import claw-os C:\WSL\claw-os .\claw-os-wsl-amd64.tar.gz --version 2
wsl -d claw-os</code></pre>
          </article>
          <article class="install-card">
            <h3>Docker</h3>
            <p>Run Claw OS in a container.</p>
            <pre><code>docker pull ghcr.io/xiaoyu-work/claw-os:latest
docker run -d --name claw --privileged ghcr.io/xiaoyu-work/claw-os
docker exec -it --user cos claw bash --login</code></pre>
          </article>
        </div>
      </div>
    </section>
  </main>

  <footer class="footer">
    <div class="container footer-inner">
      <span>Claw OS - The First Agent Native Operating System</span>
      <span><a href="https://github.com/xiaoyu-work/claw-os">GitHub</a></span>
    </div>
  </footer>
</div>
</body>
</html>
EOF
} > "$REPO_DIR/index.html"

# GitHub Pages should publish the APT repository verbatim, without Jekyll
# filtering paths that begin with underscores or rewriting generated files.
: > "$REPO_DIR/.nojekyll"

echo ""
echo ":: apt repo ready at $REPO_DIR"
echo "   suite=$SUITE component=$COMPONENT arches=$arch_list"
