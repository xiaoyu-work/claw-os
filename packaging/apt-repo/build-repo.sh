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
<meta name="description" content="Claw OS is The First Agent Native Operating System: structured OS primitives, scoped permissions, checkpoints, rollback, and a built-in agent.">
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
    radial-gradient(circle at 50% -12%, rgba(0, 92, 254, .14), transparent 34rem),
    radial-gradient(circle at 82% 18%, rgba(124, 58, 237, .10), transparent 28rem),
    linear-gradient(#fff, #fafafa 55%, #fff);
  font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  -webkit-font-smoothing: antialiased;
}

body::before {
  content: "";
  position: fixed;
  inset: 0;
  pointer-events: none;
  background-image:
    linear-gradient(rgba(0, 0, 0, .035) 1px, transparent 1px),
    linear-gradient(90deg, rgba(0, 0, 0, .035) 1px, transparent 1px);
  background-size: 48px 48px;
  mask-image: linear-gradient(to bottom, rgba(0, 0, 0, .75), transparent 60%);
}

a { color: inherit; text-decoration: none; }
code, pre { font-family: "SFMono-Regular", Consolas, "Liberation Mono", monospace; }
.page { position: relative; overflow: hidden; }
.container { width: min(1200px, calc(100% - 40px)); margin: 0 auto; }

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

.hero { padding: 86px 0 74px; text-align: center; }
.hero-logo { display: block; width: min(210px, 58vw); height: auto; margin: 0 auto 22px; }
.eyebrow {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  height: 32px;
  padding: 0 12px;
  border: 1px solid var(--line);
  border-radius: 999px;
  color: #444;
  background: rgba(255, 255, 255, .72);
  box-shadow: 0 8px 32px rgba(0, 0, 0, .04);
  font-size: 13px;
}
.pulse {
  width: 8px;
  height: 8px;
  border-radius: 999px;
  background: var(--blue);
  box-shadow: 0 0 0 5px rgba(0, 92, 254, .12);
}
h1 {
  max-width: 1120px;
  margin: 26px auto 0;
  font-size: clamp(48px, 7.4vw, 92px);
  line-height: .98;
  letter-spacing: -.06em;
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
.lead {
  max-width: 760px;
  margin: 28px auto 0;
  color: var(--muted);
  font-size: clamp(18px, 2.2vw, 24px);
  line-height: 1.45;
  letter-spacing: -.025em;
}
.actions { display: flex; justify-content: center; gap: 12px; margin-top: 34px; flex-wrap: wrap; }
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
  max-width: 1080px;
  border: 1px solid var(--line);
  border-radius: 28px;
  background: linear-gradient(180deg, rgba(255, 255, 255, .9), rgba(255, 255, 255, .72));
  box-shadow: var(--shadow);
  overflow: hidden;
}
.product-shell::before {
  content: "";
  position: absolute;
  inset: 0;
  background: radial-gradient(circle at 50% 0, rgba(0, 92, 254, .14), transparent 22rem);
  pointer-events: none;
}
.product-topbar {
  position: relative;
  height: 48px;
  border-bottom: 1px solid var(--line);
  display: flex;
  align-items: center;
  padding: 0 18px;
  gap: 8px;
  background: rgba(255, 255, 255, .72);
}
.dot { width: 12px; height: 12px; border-radius: 999px; background: #ff5f57; }
.dot:nth-child(2) { background: #febc2e; }
.dot:nth-child(3) { background: #28c840; }
.topbar-title { margin-left: 10px; color: var(--faint); font-size: 13px; }
.hero-grid {
  position: relative;
  display: grid;
  grid-template-columns: 1.15fr .85fr;
  gap: 1px;
  background: var(--line);
}
.panel {
  min-height: 360px;
  padding: 26px;
  background: rgba(255, 255, 255, .9);
}
.terminal {
  display: flex;
  flex-direction: column;
  gap: 15px;
  text-align: left;
  color: #111;
  font-size: 14px;
  line-height: 1.65;
}
.prompt { color: var(--blue); font-weight: 700; }
.terminal .muted { color: var(--faint); }
.terminal-line {
  display: grid;
  grid-template-columns: 24px 1fr;
  gap: 10px;
  align-items: start;
}
.primitive-card {
  border: 1px solid var(--line);
  border-radius: 22px;
  padding: 18px;
  background: linear-gradient(180deg, #fff, #fbfbfb);
  box-shadow: 0 18px 60px rgba(0, 0, 0, .07);
}
.primitive-title { display: flex; align-items: center; justify-content: space-between; margin-bottom: 16px; font-weight: 650; }
.badge { border: 1px solid var(--line); border-radius: 999px; padding: 4px 9px; color: var(--muted); font-size: 12px; background: #fff; }
.primitive-message {
  border: 1px solid #dbe7ff;
  border-radius: 16px;
  padding: 14px;
  background: linear-gradient(135deg, rgba(0, 92, 254, .08), rgba(255, 255, 255, .95));
  color: #1f2937;
  line-height: 1.5;
}
.primitive-list { display: grid; gap: 10px; margin-top: 14px; }
.mini {
  border: 1px solid var(--line);
  border-radius: 16px;
  padding: 13px;
  background: #fff;
}
.mini strong { display: block; font-size: 13px; margin-bottom: 4px; }
.mini span { color: var(--muted); font-size: 12px; line-height: 1.45; }

.section { padding: 98px 0; }
.section-head { display: flex; justify-content: space-between; align-items: end; gap: 30px; margin-bottom: 28px; }
.kicker { color: var(--blue); font-size: 13px; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; }
h2 { margin: 8px 0 0; font-size: clamp(34px, 5vw, 64px); line-height: .98; letter-spacing: -.055em; }
.section-copy { max-width: 510px; color: var(--muted); font-size: 18px; line-height: 1.55; }

.cards { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); border: 1px solid var(--line); border-radius: 24px; overflow: hidden; background: var(--line); box-shadow: 0 18px 70px rgba(0, 0, 0, .05); }
.feature-card { min-height: 260px; padding: 24px; background: #fff; }
.feature-card.wide { grid-column: span 2; }
.feature-card.tall { min-height: 320px; }
.icon {
  width: 36px;
  height: 36px;
  display: grid;
  place-items: center;
  border: 1px solid var(--line);
  border-radius: 12px;
  margin-bottom: 48px;
  background: linear-gradient(180deg, #fff, #f7f7f7);
  color: var(--blue);
  font-weight: 800;
}
.feature-card h3 { margin: 0 0 10px; font-size: 22px; letter-spacing: -.03em; }
.feature-card p { margin: 0; color: var(--muted); line-height: 1.55; }

.architecture {
  border: 1px solid var(--line);
  border-radius: 28px;
  background: #050505;
  color: #fff;
  overflow: hidden;
  box-shadow: 0 28px 90px rgba(0, 0, 0, .18);
}
.architecture-grid { display: grid; grid-template-columns: .95fr 1.05fr; }
.architecture-copy { padding: 38px; }
.architecture-copy p { color: rgba(255, 255, 255, .62); line-height: 1.6; font-size: 17px; }
.flow { padding: 30px; border-left: 1px solid rgba(255, 255, 255, .12); background: radial-gradient(circle at 80% 10%, rgba(0, 92, 254, .28), transparent 24rem); }
.flow-step {
  display: flex;
  gap: 16px;
  align-items: flex-start;
  padding: 18px;
  border: 1px solid rgba(255, 255, 255, .12);
  border-radius: 18px;
  background: rgba(255, 255, 255, .045);
  margin-bottom: 12px;
}
.flow-num { width: 28px; height: 28px; border-radius: 999px; background: #fff; color: #000; display: grid; place-items: center; font-weight: 700; font-size: 13px; flex: 0 0 auto; }
.flow-step strong { display: block; margin-bottom: 5px; }
.flow-step span { color: rgba(255, 255, 255, .58); line-height: 1.45; }

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
.indexes { display: flex; flex-wrap: wrap; gap: 10px; margin-top: 16px; }
.index-link {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  border: 1px solid var(--line);
  border-radius: 999px;
  padding: 9px 12px;
  color: #333;
  background: #fff;
  font-size: 13px;
}
.index-link::before {
  content: "";
  width: 7px;
  height: 7px;
  border-radius: 999px;
  background: var(--green);
}

.footer { border-top: 1px solid var(--line); padding: 34px 0; color: var(--muted); font-size: 14px; }
.footer-inner { display: flex; justify-content: space-between; gap: 20px; flex-wrap: wrap; }

@media (max-width: 900px) {
  .nav-links { display: none; }
  .hero { padding-top: 62px; }
  h1 { font-size: clamp(42px, 13vw, 70px); letter-spacing: -.055em; }
  h1 .line { white-space: normal; }
  .hero-grid, .architecture-grid, .install-grid { grid-template-columns: 1fr; }
  .cards { grid-template-columns: 1fr; }
  .feature-card.wide { grid-column: auto; }
  .section-head { display: block; }
  .section-copy { margin-top: 16px; }
  .panel { min-height: auto; }
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
        <a href="#architecture">Architecture</a>
        <a href="#install">Install</a>
        <a href="#apt">APT repo</a>
        <a href="https://github.com/xiaoyu-work/claw-os">GitHub</a>
      </nav>
      <a class="nav-cta" href="#install">Get started</a>
    </div>
  </header>

  <main>
    <section class="hero">
      <div class="container">
        <img class="hero-logo" src="assets/brand/clawos-wordmark.png" alt="Claw OS">
        <div class="eyebrow"><span class="pulse"></span> Linux, redesigned for agentic work</div>
        <h1><span class="line">The First Agent Native</span><span class="line gradient-text">Operating System</span></h1>
        <p class="lead">Claw OS exposes apps, files, browser sessions, credentials, jobs, permissions, and rollback as structured primitives that a built-in agent can use safely.</p>
        <div class="actions">
          <a class="button primary" href="#install">Install Claw OS</a>
          <a class="button" href="https://github.com/xiaoyu-work/claw-os">View on GitHub</a>
        </div>
      </div>
    </section>

    <section class="container" aria-label="Product preview">
      <div class="product-shell">
        <div class="product-topbar">
          <span class="dot"></span><span class="dot"></span><span class="dot"></span>
          <span class="topbar-title">headless cos runtime</span>
        </div>
        <div class="hero-grid">
          <div class="panel terminal">
            <div class="terminal-line"><span class="prompt">$</span><span>cos checkpoint create "clean state"</span></div>
            <div class="terminal-line"><span class="prompt">$</span><span>cos app web read https://example.com <span class="muted"># Markdown in, JSON out</span></span></div>
            <div class="terminal-line"><span class="prompt">$</span><span>cos agent ask "find risky changes and explain rollback options"</span></div>
            <div class="terminal-line"><span class="muted">-></span><span>Inspected 47 files, requested one scoped write approval, and prepared a rollback checkpoint.</span></div>
          </div>
          <div class="panel">
            <div class="primitive-card">
              <div class="primitive-title">
                <span>Headless Linux runtime</span>
                <span class="badge">WSL / Docker</span>
              </div>
              <div class="primitive-message">A Linux environment designed for agents to inspect, reason, execute, and roll back through structured <strong>cos</strong> APIs.</div>
              <div class="primitive-list">
                <div class="mini"><strong>WSL first</strong><span>Import a rootfs and run Claw OS beside your existing Windows workflow.</span></div>
                <div class="mini"><strong>Container ready</strong><span>Run the same headless OS surface in Docker or OrbStack.</span></div>
                <div class="mini"><strong>APT updates</strong><span>Keep core Claw OS packages upgraded from GitHub Pages.</span></div>
                <div class="mini"><strong>Structured primitives</strong><span>Expose files, browser reads, packages, jobs, and system state as JSON.</span></div>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>

    <section id="features" class="section">
      <div class="container">
        <div class="section-head">
          <div>
            <div class="kicker">Agent-native platform</div>
            <h2>Your computer, exposed as safe primitives.</h2>
          </div>
          <p class="section-copy">Vercel-style simplicity for an operating system: one clear surface for the agent, structured outputs for apps, and explicit control for the user.</p>
        </div>
        <div class="cards">
          <article class="feature-card wide">
            <div class="icon">01</div>
            <h3>Structured OS primitives</h3>
            <p>Use JSON-first commands for apps, files, system info, package search, browser reads, web fetches, notifications, and more.</p>
          </article>
          <article class="feature-card">
            <div class="icon">02</div>
            <h3>Built-in agent</h3>
            <p>Ask from the terminal and let the agent operate through structured headless Linux primitives.</p>
          </article>
          <article class="feature-card">
            <div class="icon">03</div>
            <h3>Scoped approvals</h3>
            <p>Risky actions are checked against capability scopes before execution, so the agent cannot silently widen access.</p>
          </article>
          <article class="feature-card">
            <div class="icon">04</div>
            <h3>Checkpoints</h3>
            <p>Create snapshots, inspect diffs, and roll back file changes after agent work.</p>
          </article>
          <article class="feature-card wide">
            <div class="icon">05</div>
            <h3>Apps with AI boundaries</h3>
            <p>Third-party apps call models through <code>cos ai chat</code> and execute tools through <code>cos ai tool</code>, keeping AI activity identity-pinned and audited.</p>
          </article>
          <article class="feature-card">
            <div class="icon">06</div>
            <h3>Local runtime</h3>
            <p><code>cos model</code> and <code>cos engine</code> manage on-device inference where available.</p>
          </article>
        </div>
      </div>
    </section>

    <section id="architecture" class="section">
      <div class="container">
        <div class="architecture">
          <div class="architecture-grid">
            <div class="architecture-copy">
              <div class="kicker">Trust architecture</div>
              <h2>Security, audit, and rollback included.</h2>
              <p>Claw OS is not a chatbot bolted onto Linux. It is a Linux-based environment where the agent talks to the computer through explicit OS contracts.</p>
            </div>
            <div class="flow">
              <div class="flow-step"><span class="flow-num">1</span><div><strong>Identity is pinned</strong><span>Apps inherit identity from the OS-spawned process tree.</span></div></div>
              <div class="flow-step"><span class="flow-num">2</span><div><strong>Capabilities are checked</strong><span>Tools bind to verbs and scopes before any side effect.</span></div></div>
              <div class="flow-step"><span class="flow-num">3</span><div><strong>Activity is audited</strong><span>Model calls and tool executions are recorded as structured events.</span></div></div>
              <div class="flow-step"><span class="flow-num">4</span><div><strong>Changes can roll back</strong><span>Checkpoints make agent work inspectable and reversible.</span></div></div>
            </div>
          </div>
        </div>
      </div>
    </section>

    <section id="install" class="section">
      <div class="container">
        <div class="section-head">
          <div>
            <div class="kicker">Install</div>
            <h2>Start with WSL or Docker.</h2>
          </div>
          <p class="section-copy">WSL and Docker are the recommended entry points today. Desktop, ISO, and VM images are experimental.</p>
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
            <p>Run the headless Claw OS environment in a container.</p>
            <pre><code>docker pull ghcr.io/xiaoyu-work/claw-os:latest
docker run -d --name claw --privileged ghcr.io/xiaoyu-work/claw-os
docker exec -it --user cos claw bash --login</code></pre>
          </article>
        </div>
      </div>
    </section>

    <section id="apt" class="section">
      <div class="container">
        <div class="section-head">
          <div>
            <div class="kicker">APT repository</div>
            <h2>Packages stay on the same GitHub Pages URL.</h2>
          </div>
          <p class="section-copy">This website is served beside the Debian repository. Existing APT clients still fetch <code>/dists</code> and <code>/pool</code> from the same base URL.</p>
        </div>
        <div class="install-card">
          <h3>Add the repo</h3>
<pre>
echo "deb [trusted=yes] https://xiaoyu-work.github.io/claw-os $SUITE $COMPONENT" \\
  | sudo tee /etc/apt/sources.list.d/claw-os.list
sudo apt update
sudo apt install claw-os-base
</pre>
          <div class="indexes" aria-label="Available package indexes">
EOF
    for a in "${binary_arches[@]}" all; do
        echo "            <a class=\"index-link\" href=\"dists/$SUITE/$COMPONENT/binary-$a/Packages\">binary-$a / Packages</a>"
    done
    cat <<EOF
          </div>
        </div>
      </div>
    </section>
  </main>

  <footer class="footer">
    <div class="container footer-inner">
      <span>Claw OS - The First Agent Native Operating System</span>
      <span><a href="https://github.com/xiaoyu-work/claw-os">GitHub</a> / <a href="dists/$SUITE/Release">Release metadata</a> / architectures: $arch_list</span>
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
