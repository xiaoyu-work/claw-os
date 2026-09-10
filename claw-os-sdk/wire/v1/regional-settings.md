# Regional settings service

The OS owns `system.regional-settings.control`. It is a session-authenticated
mutation route, not an App, account-administration shortcut, configuration-file
editor, or approval endpoint. The public SDK's explicit-binary controlled
primitive transport invokes:

```text
cos --wire=1 __regional-settings '{"action":"static_hostname","hostname":"workstation"}'
```

The CLI attaches `COS_SESSION`; callers cannot supply a session in that JSON.
The broker authenticates the process and session, rather than trusting the
environment variable. Its client authenticates the root Unix peer before
sending any request bytes, including through the root-created private Host
proxy. A fake user-owned socket cannot confirm a change.

## Closed requests and exact authority

| Action | Business fields | Requested capability | Risk |
| --- | --- | --- | --- |
| `system_locale` | `lang`, `region` | `sys.locale`, Name `system` | High |
| `owner_language` | `languages` | `sys.language`, Name `self` | Medium |
| `static_hostname` | `hostname` | `sys.hostname`, Name `static` | High |

The broker request additionally contains its authenticated session identifier.
Unknown fields/actions, owner IDs, paths, commands, bus addresses, arbitrary
locale keys, interactive-authentication flags and approval data are rejected.
These verbs do not reinterpret `sys.config` (exact Path scope) or
`sys.identity` (account administration). Standard trusted-grant containment
still applies; the provider itself requests only the exact named scope.
Role bundles are UX shortcuts, not grants or permission to elevate a launcher.

Validation occurs during typed decoding, before authority or backend access:

- `lang` and `region` are bounded ASCII POSIX locale names, at most 128 bytes,
  with optional territory, codeset and modifier. Availability remains the
  locale service's responsibility.
- `languages` preserves the colon-separated language preference, up to 4096
  bytes and 64 nonempty validated locale names.
- `hostname` is at most 64 ASCII bytes, with nonempty dot-separated labels
  of at most 63 characters, alphanumeric ends and only alphanumerics/hyphens.
  Trailing dots, whitespace, control bytes and path/command syntax are refused.

`system_locale` calls locale1 `SetLocale` with exactly `LANG=lang` and these
nine assignments using `region`: `LC_ADDRESS`, `LC_IDENTIFICATION`,
`LC_MEASUREMENT`, `LC_MONETARY`, `LC_NAME`, `LC_NUMERIC`, `LC_PAPER`,
`LC_TELEPHONE`, `LC_TIME`. As with locale1's existing setter, it replaces
system defaults; it does not rewrite running processes' environments.

`owner_language` resolves the kernel-authenticated/projected owner through
AccountsService, verifies the returned user's UID, and calls only that user's
`SetLanguage`. It neither changes another account nor copies data into a new
namespace. `static_hostname` calls only hostname1 `SetStaticHostname`.
Interactive polkit is disabled. No App or publisher identity grants an
exception, and no backend store is edited directly.

## Confirmations and errors

Each call performs one mutation. System locale and owner language are separate
operations, not an atomic transaction: success of one cannot imply success or
rollback of the other.

The provider discovers root-owned system services on the fixed system bus and
addresses their pinned unique connections. It spends the exact live capability
immediately before the setter, including for a last-use grant, then obtains an
uncached matching property readback. Success shapes are:

```json
{"action":"system_locale","status":"applied","locale":["LANG=en_US.UTF-8","LC_ADDRESS=de_DE.UTF-8","LC_IDENTIFICATION=de_DE.UTF-8","LC_MEASUREMENT=de_DE.UTF-8","LC_MONETARY=de_DE.UTF-8","LC_NAME=de_DE.UTF-8","LC_NUMERIC=de_DE.UTF-8","LC_PAPER=de_DE.UTF-8","LC_TELEPHONE=de_DE.UTF-8","LC_TIME=de_DE.UTF-8"]}
{"action":"owner_language","status":"applied","language":"de_DE:de:en"}
{"action":"static_hostname","status":"applied","hostname":"workstation"}
```

The locale response projects the ten confirmed effective settings explicitly;
an LC value omitted by localed can inherit matching `LANG`. Missing,
conflicting or duplicate readback values do not confirm a write.
No response grants capabilities or proves installation/approval.

An unavailable pre-dispatch attempt still spends one exact live grant use
before the broker releases that diagnostic, satisfying the existing audited
request obligation. Successful discovery does not spend early: its last-use
grant remains available at the mutation gate. Invalid input, missing authority
and revocation do not gain a success path through error handling.

Before-dispatch unavailability is `unavailable` / CLI `KERNEL_UNAVAILABLE`;
authority/backend permission refusal is `not_authorized` /
`PERMISSION_DENIED`. A timeout, lost acknowledgement, unexpected setter
failure, failed readback or mismatching post-write state is `indeterminate` /
`INDETERMINATE`: the effect may have applied. There is no automatic retry,
success fallback or fabricated rollback. Request audit records project bounded
locale/language/hostname identifiers; overlong or non-identifier values are
redacted. Actual grant consumption uses the existing authority audit and
mutation journal.

## Rollout boundary

The generic primitive transport exists in published SDK 1.0.0. The three new
capability names are a source-level vocabulary addition and require a subsequent
real SDK publication for consumers of its manifest schema. Do not modify the
immutable 1.0.0 archive, pin a fake release, or import a mutable OS checkout.

Settings' GUI operation declarations must request only its actual regional
needs. Such declarations neither acquire missing High-risk authority nor
bootstrap Admin. Authenticated GUI-session admission and root-owned launch
custody remain prerequisites. Neither broad Settings polkit rules nor the
separate custom Users policy may be removed merely because isolated provider
and renderer tests pass.

Implementation and private-fixture commands:
[`core/src/clawd/MODULE.md`](../../../core/src/clawd/MODULE.md).
