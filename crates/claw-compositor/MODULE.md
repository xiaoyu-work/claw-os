# Native Compositor Authority

## Responsibility

Provide the OS-private native compositor enforcement bridge for Hyprland.
It consumes the existing [display control contract](../claw-display-control/MODULE.md);
it does not replace the Root broker or make App capability decisions.

The lease/origin rules preserve the existing
[COSMIC enforcement](../../desktop/comp/src/display_authority.rs), including
Root-only creator descriptors, actual process/cgroup identity, single-use
context creation, independent selection read/write and panel rights, finite
leases and checked retirement. This implementation is GPL-3.0-only; the
shared Apache-2.0 protocol crate and the Agent remain separate.

## Files and ownership

| File | Purpose |
| --- | --- |
| `src/authority.rs` | Authenticated Root command processing, immutable grants, native resource accounting and correlated replies |
| `src/ffi.rs` | Bounded OS-private C ABI and event-thread confinement |
| `test/unit/authority.rs` | Grant separation, one-shot context, retirement and expiry regressions |
| `../../desktop/hyprland-claw/include/claw_compositor.h` | Native declaration and ownership contract |
| `../../desktop/hyprland-claw/test/abi.cpp` | Real C++ linkage and unauthenticated-startup refusal |

Only the protected compositor receives its Root activation descriptor.
An App ID, string label, native token supplied by an App, environment claim
or a successful FFI call is not a grant. Native code must obtain an origin
from the checked connection/context path and recheck rights before disclosure
or effects.

The native adapter must report actual client/listener addition and destruction.
Retirement is acknowledged only after those counts are empty. Root selection
payloads retain sealed, bounded MIME transfer semantics and do not inherit an
App lifetime. The native adapter must still enforce read checks on offers and
transfers; owning a payload does not authorize its disclosure.

The bridge is not yet linked into the production desktop. Native hooks,
actual authenticated-display acceptance and package activation remain
separate implementation requirements.

## Validation

From the OS repository root:

```bash
cargo test -p claw-compositor -- --test-threads=1
cargo clippy -p claw-compositor -- -D warnings
cargo build -p claw-compositor
clang-format --version
clang-format --style=file:desktop/hyprland/.clang-format --dry-run --Werror \
  desktop/hyprland-claw/include/claw_compositor.h \
  desktop/hyprland-claw/test/abi.cpp
c++ -std=c++20 desktop/hyprland-claw/test/abi.cpp \
  target/debug/libclaw_compositor.a -ldl -lpthread -lm -o target/claw-compositor-abi
target/claw-compositor-abi
```

When `CARGO_TARGET_DIR` is set, use that target directory for the static
library and smoke executable instead. A successful ABI smoke test proves
linkage and refusal only, not a rendered or authenticated GUI session.
The imported upstream formatting rules require a compatible formatter;
23.1.5 is the verified version. Older distribution formatters may reject the
upstream `PackArguments` option rather than checking the files.
