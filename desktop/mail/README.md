# Claw Mail source

The Thunderbird **153.2.0esr product source is copied into this repository** at
[`comm/`](comm/), including its Mail UI, account and message engines, compose
implementation, calendar, libraries and upstream tests. It is not a submodule
or another wrapper around the installed application.

See [PROVENANCE.md](PROVENANCE.md) and [upstream.json](upstream.json) for exact
source and platform revisions. This is the source-import part of
[Mail milestone M3](../../docs/app-product-redesign.md), not a claim that the
Mail product migration, native MCP interface or installed package is complete.

## Build layout

Thunderbird's build runs from the matching Firefox platform directory with
the product tree at `comm/`:

```text
Firefox platform (pinned build dependency)
  mach
  mozconfig
  comm/  <-- this repository's desktop/mail/comm source
```

Use Linux or WSL with source and build output on the Linux filesystem.
The platform pin is not interchangeable with an installed Firefox binary.
The upstream [build guide](https://developer.thunderbird.net/thunderbird-development/building-thunderbird)
documents `mach` and prerequisites. The application selection is
`ac_add_options --enable-project=comm/mail`; omitting it builds Firefox.

**Current status:** complete source import; native build integration and
packaging are in progress. No forked binary is installed by this commit.
The existing COSMIC `desktop/justfile` does not build this Mozilla project.

## Product integration

Changes should go directly into `comm/mail/` and `comm/mailnews/`, reusing the
existing account, message store and compose services. UI and future Agent MCP
must use those same services and explicitly selected profiles, not separate
IMAP/SMTP clients or mirrored mailbox databases.

The source-first direction supersedes the uncommitted external-extension
reference-layer experiment. `email`, `mail-ai` and `gateway-email` remain
installed identities until their complete data, permission and package cutover.
Do not merge their grants, add compatibility aliases, or expose an existing
profile merely because its files are reachable.
