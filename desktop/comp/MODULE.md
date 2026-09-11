# OS Compositor

This GPL COSMIC fork keeps its original [license](LICENSE) and
[component provenance](../PROVENANCE.md). It owns Wayland protocol dispatch,
not App permissions, login authentication or provider implementations.

The compositor starts only through the
[Root display activation](../../core/src/display_session/MODULE.md). Its
private control descriptor and authenticated parent binding are required before
protocol dispatch. The former owner-created `COSMIC_SESSION_SOCK` exchange is
removed; `session.rs` retains only the in-process environment projection.

`src/display_authority.rs` consumes Root creator leases and records accepted
clients and listeners. `context_created` consumes an installed creator once;
client-supplied engine/App/instance strings cannot bind authority.
Ordinary login clients have a separate kernel-login origin, never App selection
rights. Layer-shell admission is separate from selection and cannot come from
a Panel label.

`src/wayland/handlers/selection.rs` applies independently granted read/write
rights to the [pinned Smithay hooks](vendor/README.md). Unbound, retired or
expired instances fail before offers, MIME disclosure, payload forwarding or
selection mutation. All four selection protocols are covered; DnD is separate.
Retirement removes listeners and all accepted clients, while Root confirms
the actual instance and descendant teardown.

The private headless workspace under `test/display-control/` embeds these
production authority/security-context handlers. Its synthetic native client
is run by the actual Root Host and full worker sandbox. Its compositor-side
write witness verifies real write-only data and clears without giving the
writer read authority. No real desktop, clipboard/history or renderer backend
is used. Existing dispatch/privacy/default-allow regressions remain under
`test/selection-read/`.

From the OS repository root:

```bash
cargo build --manifest-path desktop/comp/Cargo.toml --locked
cargo test --manifest-path desktop/comp/test/selection-read/Cargo.toml --locked
cargo build --manifest-path desktop/comp/test/display-control/Cargo.toml --locked
cargo clippy --manifest-path desktop/comp/test/display-control/Cargo.toml \
  --locked --all-targets -- -D warnings
```

The production build requires its normal native development libraries.
Headless dispatch success does not claim real greetd/logind/KMS, visual,
X11, CopyQ-history or every native producer's permission acceptance.
