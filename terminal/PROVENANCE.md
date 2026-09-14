# Codex frontend provenance

The presentation frontend is **OpenAI Codex**, not a reimplementation with
similar widgets.

- Public source: <https://github.com/openai/codex>.
- Exact revision: `a592c38c16cdd7623dacc9168926ebccedfb67d3`.
- Authoritative maintained pin and input checksums: [`source.json`](source.json).
- Upstream license:
  [Apache-2.0 LICENSE](https://github.com/openai/codex/blob/a592c38c16cdd7623dacc9168926ebccedfb67d3/LICENSE).
- Upstream attribution:
  [NOTICE](https://github.com/openai/codex/blob/a592c38c16cdd7623dacc9168926ebccedfb67d3/NOTICE),
  including Ratatui's MIT attribution.
- Upstream source modifications: the digest-pinned
  [remote-startup and Claw exit-hint patches](patches/README.md). They suppress
  announcement preloading and both local auth/cloud-loader initializations for
  explicit remote frontends and replace retired-socket exit hints with durable
  conversation commands only when `COS_TUI_FRONTEND=1`. Rendering, nonremote
  startup, and unflagged exit behavior are retained.

The builder exports the complete pinned Git tree rather than copying only the
TUI crates: its large upstream workspace and compile-time resources remain
intact. Git object extraction avoids Windows checkout line-ending conversions,
untracked files, and ignored developer output. Cargo uses the upstream lockfile
with `--locked` and the default feature set. Maintained patches are checked and
applied from verified in-memory bytes before compilation. Checksum-based source
synchronization repairs changed cache files while preserving timestamps of
unchanged files for incremental builds.

The build recipe explicitly selects Rust 1.98.0; upstream's unchanged toolchain
file specifies 1.95.0. Both versions are recorded in the pin. This is a source
and build-recipe pin, not a claim of bit-for-bit reproducibility across OS
libraries, compiler environments, or architectures. The generated receipt
records the actual compiler, target, revision, and artifact digest. The build
supplies upstream's `STABLE_GIT_COMMIT` environment hook; the CLI's `--version`
still reports the snapshot package version `0.0.0` at this pin.

The pin separately records the staging policy: matching-target GNU binutils
`--strip-debug` runs on a copy, without changing upstream Cargo configuration
or its unstripped output. The receipt records both the staging input digest and
the final shipped digest, along with the strip tool identity/version. Only the
final bytes pass packaging verification. Verified same-source bundles can be
restaged when this policy changes; source/patch/compile identity cannot change
through that path.

## Runtime attribution

`build/agent-tui/share/codex-tui` contains the exact upstream `LICENSE` and
`NOTICE`, additional in-tree third-party notices, the source pin, applied patch
files, a `MODIFICATIONS` notice, and a build receipt. Distribute this entire
directory with the private binary. It is
generated from the verified source, not downloaded separately from a moving
branch. Do not commit build caches or binaries.

The schema-v2 receipt binds the binary's SHA-256, actual ELF target, all
attribution files, patch digests, and the canonical checked-in build/staging
recipe. Before
packaging, run `python3 -B terminal/build.py --verify-artifact --target TRIPLE`;
it fails rather than accepting unmarked, stale, wrong-architecture, or modified
outputs. See the [packaging API](README.md#packaging-api) for exact paths and
copying requirements. Package signatures remain the distribution trust boundary.

## Updating the pin

Review the public commit and changes to CLI options, the TUI remote transport,
app-server schemas, local authentication/configuration, dependencies, helper
assets, and notices together. Update the canonical Git-blob checksums and
build recipe in `source.json`, rebuild on Linux, and repeat the remote startup
checks against the Claw adapter. Never replace the pinned frontend with a
smaller renderer to work around a build failure.
