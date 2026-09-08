# Mail Product Source

## Responsibility

This directory owns the in-repository Thunderbird product fork. Mail account,
folder, message, compose and draft integration must be implemented in this
source, rather than in a second mailbox client or an expanding external
extension wrapper.

## Key files

| Path | Role |
| --- | --- |
| `comm/` | Complete, initially unmodified Thunderbird release source |
| `upstream.json` | Immutable Thunderbird and matching Firefox platform pins |
| `PROVENANCE.md` | Origin, licensing, trademarks and security-update obligations |
| `README.md` | Source layout, build status and integration boundary |
| `comm/mail/` | Mail UI, startup, modules and application configuration |
| `comm/mailnews/` | Account, folder, message-store and compose engine |
| `comm/build/moz.configure/gecko_source.configure` | Required Firefox platform pairing |

## Boundaries

The Firefox platform is a pinned build dependency, not a second Mail product.
Product changes belong in the checked-in `comm/` tree. No git submodule or
runtime download replaces that tree.

Model credentials, AI authorization and system privileges stay behind the
existing core/SDK boundaries. Importing source does not grant an Agent access
to an existing user's Thunderbird profile or merge the old App identities.

## Validation

Use upstream `mach` build and component-specific tests from a Linux filesystem.
Do not run the whole upstream suite through the repository's Python App runner.
Keep upstream tests and licenses intact, even when they are not default Claw OS
CI inputs. See [README.md](README.md) for current build status.
