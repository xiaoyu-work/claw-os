# Nearby transport provenance

The Claw wrapper is OS-owned source. Its radio, AWDL and AirDrop implementations
are MIT-licensed dependencies of [Owly](https://github.com/t4t5/owly),
revision `7ff594a61e1e158fe56f22b9efb9d8ea88dd918b` (0.4.1).
`OWLY-LICENSE` preserves the complete upstream notice. Agent packaging ships
both this provenance and that license beside the installed binaries.

`Cargo.toml` fixes one upstream revision, and this independent workspace's
`Cargo.lock` fixes its complete graph. Package builds use `--locked`.
Only [`vendor/owlyd`](vendor/owlyd/PROVENANCE.md) is locally patched: the upstream
lease server otherwise reports successful retirement after a failed network
restore. The patch propagates that failure instead; the other Owly crates are
unmodified Git dependencies.
This is a tested OS transport dependency, not an App source selector:
`packaging/apps.lock.json` continues resolving App `main` once per build.
Neither an installed daemon nor a model downloads transport code at runtime.

The systemd radio sandbox, NetworkManager device section, and awdl0 link policy
are adapted from that revision's `packaging/` integration. Claw changes the
executable, update gate, service ownership and activation. The upstream
active-user polkit acquisition policy is **not** installed: only Root can call
the radio D-Bus API. The standalone `owly` Debian package conflicts because it
owns the same D-Bus service name; the source checkout is unaffected.

`claw-nearby-worker` uses the protocol libraries in a non-root, bounded
namespaced process. It has no system bus, home, Agent/provider credentials or
arbitrary executable selector. `claw-nearby-radio` contains only the lower radio
layer and lease service; no AirDrop archive/HTTP parser runs as Root.

Owly's card coverage and Everyone-only interoperability remain upstream
limitations. Software fixtures, package inspection and compilation are not
physical iPhone/Wi-Fi acceptance or a hardware certification report.
