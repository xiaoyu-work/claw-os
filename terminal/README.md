# Upstream Codex TUI for Claw OS

This module builds the **actual OpenAI Codex CLI/TUI** at the revision recorded
in [`source.json`](source.json), with its complete upstream Cargo workspace,
default features, and a [minimal maintained remote-startup patch](patches/README.md).
It does not put upstream crates in Claw's Rust workspace or
replace Codex's rendering with a lookalike. See [provenance](PROVENANCE.md) and
the [module guide](MODULE.md).

The executable is private presentation machinery. **The Claw Agent remains the
only execution backend.** Building upstream app-server/core dependencies does
not select them as a runtime backend.

## Build

Run from the Claw OS repository root on Linux, or inside Ubuntu under WSL:

```bash
bash terminal/build.sh --target x86_64-unknown-linux-gnu --jobs 4
```

The build uses Python 3.12+, Git, GNU tar, rsync, matching-target GNU binutils,
and Rust 1.98.0 through `rustup run`, with
upstream's locked dependencies. It does not install system packages or
toolchains automatically. Follow missing-dependency errors before installing
anything; the wrapper resolves Linux HOME from `getent passwd` and explicitly
sets PATH to the Linux Cargo/system tools.

For practical cold-build performance, keep the repository and its ignored build
cache on a Linux filesystem. WSL's Windows-backed mounts make the large
upstream dependency tree's per-file I/O substantially slower.

For development, an existing **clean checkout at the exact pinned commit** can
replace the public Git fetch:

```bash
bash terminal/build.sh --source ../codex --target x86_64-unknown-linux-gnu --jobs 4
```

This option does not copy the mutable checkout. It verifies HEAD and cleanliness,
then extracts immutable pinned Git objects. The checkout may contain ignored
Cargo output, which is not copied. Without `--source`, the builder fetches the
same exact revision from the public repository. `--prepare-only` performs source
acquisition and input verification without compiling.

The checkout must be clean **as seen by Linux Git**. A Windows checkout using a
different global `core.autocrlf` policy can appear modified inside WSL; use the
default public acquisition instead of weakening source verification or changing
the user's checkout.

Everything generated stays under ignored `build/agent-tui`: the bare source
cache, extracted tree, Cargo download/target caches, scratch files, and staging.
Each build exports a fresh pinned tree, applies the digest-verified patches,
then checksum-syncs it into the stable source cache. Unchanged source timestamps
remain untouched; modified or extra cache files are repaired/removed instead of
trusting a cache tag. The build uses an exclusive cache lock and does not share
Claw's Cargo target.

## Packaging API

Use an explicit Rust target in both commands, from the repository root:

```bash
bash terminal/build.sh --target x86_64-unknown-linux-gnu --jobs 4
python3 -B terminal/build.py --verify-artifact --target x86_64-unknown-linux-gnu
```

`bash terminal/build.sh --verify-artifact --target ...` is equivalent. Supported
triples are `x86_64-unknown-linux-gnu` (Debian `amd64`) and
`aarch64-unknown-linux-gnu` (Debian `arm64`). For ordinary development builds,
omitting `--target` selects the Rust compiler's host; **verification requires
`--target`** so packaging cannot accept the wrong architecture by omission.
Cross-builds require the target's Rust standard library, linker, and native
dependency/sysroot configuration. There is no automatic host-target fallback.
Prefer native per-architecture build runners.

The pinned staging recipe runs **`--strip-debug` on the staging copy only**,
using `x86_64-linux-gnu-strip` or `aarch64-linux-gnu-strip`. The producer checks
GNU tool identity and its advertised ELF format before use, rechecks the output
architecture, and rejects remaining non-allocated debug sections. Missing or
wrong-target strip tools are fatal. Cargo's target binary and release profile
remain unchanged.

Pinned stdlib-only Rust patch checks run before Cargo. They cover the same
exit-hint policy used by the TUI for both Claw-marked and ordinary Codex output,
without model requests or a separate dependency build.

Verification is read-only: it does not execute the binary, fetch sources, invoke
Cargo, or repair/relabel a stale artifact. It exits nonzero on missing files,
obsolete markers, a mismatched checked-in source pin/build recipe, a wrong
marker target or ELF machine/class, changed executable bytes, missing/changed
attributions, a mismatched staging recipe, retained non-allocated debug sections,
or symlinked bundle paths. It also refuses verification while
the builder holds the cache lock.

On success, stdout is one JSON object containing the receipt plus absolute
`binary`, `assets_directory`, and `provenance` paths. Errors go to stderr with
no success JSON. An explicitly downloaded prebuilt bundle can be checked using
`--artifact-root PATH`; its layout must match the output table below. Verification
always uses **this checkout's** `terminal/source.json`, not merely the pin
supplied inside a downloaded bundle.

When only the staging recipe changes, an existing verified same-source bundle
can be restaged without invoking Cargo or acquiring sources:

```bash
bash terminal/build.sh --restage-verified --target x86_64-unknown-linux-gnu
python3 -B terminal/build.py --verify-artifact --target x86_64-unknown-linux-gnu
```

This first verifies the input against its recorded pin, then requires the current
upstream identity, patch set, Cargo recipe, and attribution/layout contract to
match exactly; only `staging` may differ. `--artifact-root PATH` can select the
input bundle. Output remains `build/agent-tui`. The producer strips a fresh
staging copy and emits a new receipt, never modifying the verified input in
place or blessing a different source build.

Publication jobs should upload only `bin/` and `share/` from `build/agent-tui`.
Packaging must propagate verification failures; never fall back to a `codex`
found on PATH, another build tree, or an unmarked older executable. Copy the
verified binary and the entire attribution directory without changing their
bytes. Installing modes/owners is fine; stripping or otherwise rewriting the
executable invalidates the receipt's digest and requires a controlled build
recipe/staging change, not hand-editing the marker to bless arbitrary bytes.
This marker binds trusted build outputs to the pin; it is not a substitute for
the distribution's package signatures.

Install the complete artifact layout under `/usr/lib/cos/tui`:

- Executable: `/usr/lib/cos/tui/bin/codex-tui`.
- Attribution/provenance tree: `/usr/lib/cos/tui/share/codex-tui`.

Do not create a compatibility symlink or a duplicate root-level executable.
Package assembly verifies the source bundle, copies `bin/` and `share/`
unchanged, then verifies the copied package tree before assembly. From the
repository root, run the first check before copying and the second afterward:

```bash
python3 -B terminal/build.py --verify-artifact --target "$TRIPLE"
# Copy bin/ and share/ unchanged into staging/usr/lib/cos/tui before continuing.
python3 -B terminal/build.py --verify-artifact --target "$TRIPLE" \
  --artifact-root staging/usr/lib/cos/tui
```

Both checks are mandatory; the second prevents a changed build output or copy
error from silently entering the package. No stripping follows either receipt.

## Validation and outputs

The builder's standalone regressions require no third-party Python packages:

```bash
python3 -B -m unittest discover -s terminal -p 'test_*.py'
```

After building, run the actual binary's credential-free startup probe in a
network namespace (requires Linux user/network namespace support):

```bash
unshare --user --map-root-user --net python3 -B terminal/smoke.py
```

WSL's Windows-backed filesystem does not support Unix socket files. For a
checkout there, use a small project-local tmpfs fixture in a **private mount
namespace**, not a system temporary directory:

```bash
unshare --user --map-root-user --mount --net \
  python3 -B terminal/smoke.py --project-tmpfs
```

The 64 MiB fixture exists only below `build/agent-tui`, is unmounted and removed
afterward, and does not alter other processes' mount or network namespaces.

It checks real CLI flags, fail-closed explicit socket failure, the Unix WebSocket
handshake, and honest auth-free startup through `config/read`. Its small socket
fixture rejects further operations; it neither implements a backend nor submits
any model turn. Fixture files are removed afterward.

Outputs:

| Path under `build/agent-tui` | Purpose |
| --- | --- |
| `bin/codex-tui` | Linux ELF built from upstream `codex-cli`'s `codex` target |
| `share/codex-tui/LICENSE` and `NOTICE` | Exact upstream license and attribution |
| `share/codex-tui/third_party/` | Additional upstream third-party notices |
| `share/codex-tui/source.json` | Source/build pin for the shipped frontend |
| `share/codex-tui/build-info.json` | Compiler, target, revision, artifact digest |
| `share/codex-tui/patches/` and `MODIFICATIONS` | Exact applied patches and downstream change notice |

The provenance marker is `share/codex-tui/build-info.json`, with
`schema_version: 2` and `artifact_kind: "claw-agent-tui"`. It records the exact
`upstream_revision`, `source_pin_sha256`, `cargo_recipe`, `staging_recipe`,
requested `target`, the target-qualified GNU `strip_tool` identity/version,
`staging_input_sha256`,
`binary_sha256`, and a `license_sha256` entry for every declared attribution
file, alongside compiler/build diagnostics and the ordered `patches` digest
records. Packaged patches and the modification notice are also verified.
The pin hash is SHA-256 of the
source JSON serialized with sorted keys and compact separators; use the verifier
rather than duplicating this logic in packaging.

Exact attribution paths below `share/codex-tui`:

- `LICENSE`
- `NOTICE`
- `third_party/voice/NOTICE.md`
- `third_party/wezterm/LICENSE`
- `codex-rs/vendor/bubblewrap/LICENSE`

Keep `source.json`, `build-info.json`, `MODIFICATIONS`, and `patches/` with these
files.

Upstream's snapshot package version is `0.0.0`; `--version` and the initialization
handshake report that value at this pin. The builder supplies `STABLE_GIT_COMMIT`
to upstream build hooks, but use the source pin and receipt's artifact digest,
not the CLI version string, to identify this build.

The upstream release profile retains debug information in the Cargo target.
The installed artifact is a separate `--strip-debug` copy, hashed only after
that transformation. Small allocated helper sections such as
`.debug_gdb_scripts` are intentionally retained, matching GNU strip semantics.
Parent packaging must not strip the verified output a second time.

GNU/Linux builds inherit their build host's native-library ABI. Build release
packages on their supported distribution baseline, and inspect `ldd` and
`readelf --version-info` on the executable before shipping it to older systems.
The target triple alone does not guarantee compatibility with an older glibc.

## Required launcher contract

The Claw launcher is responsible for creating and supervising an owner-private
local adapter socket and a dedicated, owner-private UI state directory. For
example, once those already exist, the private invocation has this shape:

```bash
COS_TUI_FRONTEND=1 \
CODEX_HOME="${XDG_STATE_HOME:-$HOME/.local/state}/cos/agent-tui" \
  /usr/lib/cos/tui/bin/codex-tui \
  --remote "unix://${XDG_RUNTIME_DIR:?set XDG_RUNTIME_DIR}/cos/agent-tui/app-server.sock" \
  --cd "$verified_owner_home" \
  --sandbox danger-full-access \
  --ask-for-approval on-request \
  -c 'cli_auth_credentials_store="ephemeral"' \
  -c check_for_update_on_startup=false \
  -c analytics.enabled=false \
  -c feedback.enabled=false \
  -c 'otel.exporter="none"' \
  -c 'otel.trace_exporter="none"' \
  -c 'otel.metrics_exporter="none"' \
  -c otel.log_user_prompt=false
```

The launcher must resolve unset XDG directories safely, validate ownership and
permissions, reserve these arguments rather than forwarding arbitrary upstream
subcommands, and preserve the same explicit endpoint across reconnect/resume.
Never invoke this upstream CLI without `--remote`; never retry without it.
Do not use empty `unix://`, which means the upstream daemon's default socket.
Keep the auth-store, update, and telemetry overrides launcher-controlled rather
than trusting mutable UI preferences to preserve them.

`COS_TUI_FRONTEND=1` is a child-only **presentation marker**, never a backend or
authority selector. It changes exit guidance to
`cos agent chat --session <presentation UUID>` and tells users to resume that
conversation and press Esc to stop its current turn. Reconnecting to the old
private socket would be guaranteed to fail after its owning `cos` exits.
Without the marker, upstream Codex exit formatting remains unchanged.

For the current Claw adapter, `$verified_owner_home` is the authoritative owner's
verified home, not an arbitrary model-supplied or inherited HOME value. Real
per-task cwd support remains a backend responsibility. The legacy Codex
`danger-full-access` setting here disables Codex-managed sandbox claims; it
**does not grant Claw capabilities or bypass Claw approval/policy gates**.
The backend reports its actual external enforcement and rejects requested
narrowing it cannot enforce. Do not substitute the combined
`--dangerously-bypass-approvals-and-sandbox` flag.

- **Transport:** HTTP WebSocket upgrade over AF_UNIX, with request URI
  `ws://localhost/rpc`; each JSON-RPC message occupies a WebSocket message.
  This is **not** newline-delimited JSON or Claw's broker framing.
- **Handshake:** `initialize` followed by `initialized`; the client identifies
  as `codex-tui`, sends package version `0.0.0` at this pin, and requests
  `experimentalApi: true`. The adapter must implement the pinned app-server
  protocol, including server requests and notifications.
- **Features:** no extra Cargo feature is required for Unix remote mode.
  `--remote-transport` is an **exec-server** option, not an interactive TUI
  option. Do not pass it.
- **Socket authentication:** `--remote-auth-token-env` supports only eligible
  WebSocket URLs and is rejected for Unix sockets. Use private ownership,
  permissions, and adapter-side peer checks instead.
- **Failure:** explicit remote connection errors propagate. The different,
  implicitly discovered `LocalDaemon` path can fall back to embedded Codex;
  the launcher must never select that path.

These contracts are implemented upstream in `codex-rs/cli/src/main.rs`,
`codex-rs/tui/src/{lib,startup_orchestration,app_server_connection}.rs`, and
`codex-rs/app-server-client/src/remote.rs` in the pinned source.

## Configuration and honest authentication

`CODEX_HOME` is **UI-local state**, not the user's ordinary `~/.codex` and not
Claw's canonical conversation store. Never copy or expose user Codex credentials
to it. Use the supported **ephemeral** auth store: it has no persistent
file/keyring fallback, unlike file-only mode which can load a residual
`CODEX_HOME/auth.json`. Do not run `codex login`.

Remove these only from the **frontend child's** environment, not from the
launcher/adapter process or the Claw backend:

- `OPENAI_API_KEY`, `CODEX_API_KEY`, and **`CODEX_ACCESS_TOKEN`**. The access-token
  path is checked even when Codex API-key environment loading is disabled and
  before the ephemeral-store early return.
- `OPENAI_FEDERATION_RULE_ID`, `OPENAI_IDENTITY_TOKEN_FILE`, and
  `OPENAI_WORKLOAD_IDENTITY_CONTEXT`. Either of the first two selects workload
  identity; explicit remote mode rejects that local selection.
- Inherited `CODEX_EXEC_SERVER_*` routing/authentication settings, including
  `CODEX_EXEC_SERVER_URL` and the `NOISE_REGISTRY_URL`, `NOISE_ENVIRONMENT_ID`,
  `NOISE_AUTH_TOKEN`, and `NOISE_CHATGPT_ACCOUNT_ID` variants. Environment
  preparation still runs in the frontend and can otherwise connect to an
  unrelated executor. Unset these rather than setting the URL to `none`, which
  also disables local environment access.

Do not inherit the backend's telemetry configuration into the UI process.
Frontend-only removal of `OTEL_*` settings is additional defense in depth;
the explicit exporter overrides below are still required. Preserve normal
terminal/editor/display environment needed by local UI features.

Recommended launcher-owned local `config.toml` settings:

```toml
cli_auth_credentials_store = "ephemeral"
check_for_update_on_startup = false

[analytics]
enabled = false

[feedback]
enabled = false

[otel]
exporter = "none"
trace_exporter = "none"
metrics_exporter = "none"
log_user_prompt = false
```

Authentication is determined by the adapter's `account/read`, not a fake local
API key. For a Claw-managed backend that needs no OpenAI login, return:

```json
{"account": null, "requiresOpenaiAuth": false}
```

The upstream TUI then skips OpenAI login onboarding. Do not fabricate a ChatGPT
account, subscription tier, or entitlement to unlock account-gated screens.
The adapter's config/model/thread/history responses must describe Claw truthfully;
unsupported operations must fail explicitly rather than run Codex.

`check_for_update_on_startup = false` is the exact upstream setting. At this
pin, `tui/src/updates.rs:27` and `:151` return before both background update
checking and the popup path. Keep this launcher-enforced even though the current
`0.0.0` source-build version also skips checks: the paired Claw package, not
Codex's GitHub/npm/Homebrew update machinery, owns frontend updates.

`analytics.enabled = false` only disables the metrics exporter. Independent
OTEL log and trace exporters remain possible unless explicitly set to `none`
(`core/src/otel_init.rs:68`, `otel/src/provider.rs:194`). Ephemeral auth is
defined in `config/src/types.rs:111`; its no-persistent-fallback branch follows
`CODEX_ACCESS_TOKEN` processing in `login/src/auth/manager.rs:1509-1535`.
These paths refer to the pinned upstream tree.

## Scope and upstream limitations

This module supplies sources and a build artifact, not a proof of backend
feature parity. Claw's launcher, app-server adapter, session service, policy
mapping, packaging, and end-to-end feature tests are separate responsibilities.

Remote mode retains the real composer, rendering, key handling, pickers, and
protocol-driven screens. However, the pinned upstream explicitly rejects
`--worktree` for remote sessions, and account-gated OpenAI services are not
available merely because the TUI was built.

The upstream realtime voice UI additionally requires a separately prepared
`codex-resources/voice` runtime (including `codex-voice-host`, native GStreamer
libraries/plugins, and runtime metadata). Building the CLI alone does **not**
provide that runtime. Its upstream recipe is in `codex-rs/voice-host/README.md`
and `third_party/voice` in the pinned tree; do not claim voice support without
building/staging it and implementing the corresponding Claw backend protocol.

## Automatic startup and remaining cloud behavior

The maintained patch prevents explicit-remote announcement preload and both
local cloud-config/auth loader initializations. Nonremote behavior is retained.
Together with the enforced frontend settings/environment, a fresh remote UI
does not need upstream cloud work to initialize. This is **not an egress
boundary** and does not remove user-triggered UI features.

| Surface at the pinned revision | Behavior and required control |
| --- | --- |
| `tui/src/lib.rs:1062`, `tui/src/tooltips.rs:249` | Upstream preloads `https://raw.githubusercontent.com/openai/codex/main/announcement_tip.toml`. The maintained patch skips that call in explicit remote mode, without deleting the tooltip UI or its bundled fallback text. |
| `tui/src/lib.rs:959`, `tui/src/startup_orchestration.rs:261,381` | Upstream initializes local cloud-config/auth loaders even for remote sessions. The patch bypasses both initialization sites before local auth lookup. Parent-side child credential filtering and ephemeral auth remain required defense in depth. |
| `tui/src/pets/asset_pack.rs:29,45`, `chatwidget/pets.rs:24`, `app/pets.rs:78` | Built-in pets fetch missing spritesheets directly from `https://persistent.oaistatic.com/codex/pets/v1`, on selection or restoration of an uncached configured pet. Those spritesheets are not included in the Git tree or this source-only build. Preserve the feature by prebundling/preseeding assets with verified provenance/hashes/attribution, or broker their acquisition through Claw; do not quietly replace or drop the UI. |
| `tui/src/app/history_ui.rs:222`, `tui/src/inline_visualization/viewer.rs:16` | On-demand browser-opening actions can leave the frontend process. Visualization HTML also permits CDN scripts/styles/fonts/images. Gate external URL actions through Claw and supply approved/offline assets where required. A network restriction on the TUI alone does not necessarily constrain an already-running external browser. |

The Daybreak eligibility fetch already excludes remote workspaces
(`tui/src/daybreak.rs:31`), so it is not an additional remote-startup request.
Feedback uploading already uses the app-server `feedback/upload` RPC
(`tui/src/app/background_requests.rs:1313`); the Claw adapter must reject or
implement that operation itself rather than invoking upstream Sentry upload.

Any egress boundary must apply to the frontend/its helpers without cutting off
the Claw adapter/backend's network or credentials. Preserve the private Unix
connection and intentionally supported local terminal/editor/browser
integrations. The offline smoke probe validates startup under a private network
namespace; it does **not** prove the unmodified frontend makes no outbound
attempts, nor establish that every desktop integration works in that namespace.

To validate the patched startup against Internet connect/send attempts, install
`strace` and `iproute2` when required and run from the repository root:

```bash
unshare --user --map-root-user --mount --net \
  python3 -B terminal/smoke.py --project-tmpfs --check-no-cloud
```

This adds an unrouted documentation-only network interface in another private
network namespace, so DNS address-family detection cannot hide an attempted
request just because there are no interfaces. It uses an inert, invalid
access-token sentinel, leaves time for background startup tasks, and fails on
Internet connect/send attempts before stopping at `config/read`. No real
credentials or model turns are used. The unpatched binary is the negative
control; user-triggered pet/browser/voice networking is outside this probe.
