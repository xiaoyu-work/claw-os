# Broker Client and System Review Presentation

`clawd-client` is an unprivileged broker client. It owns bounded framing,
correlation, deadlines and socket-peer checks, not authorization, capabilities,
provider implementation, desktop state or a durable approval store.

## System review contract

[`src/system_review.rs`](src/system_review.rs) defines the closed version-1
presentation DTO, action/choice labels and read-only terminal formatter.
The OS projects authenticated subjects and catalog labels into this contract;
App-supplied purposes and affected functions remain visibly separate.
Installation, activation and update confirmations are not capability grants.
The contract supports headless subjects without a desktop manifest.

| Surface | Request | Response |
| --- | --- | --- |
| `system.review.pending` | `{limit}` | `PendingReviews { reviews }`, including existing capability requests |
| `system.review.show` | `{id}` | `SystemReview` |
| `system.review.cancel` | `{review: ReviewDecision}` with `action: cancel` and no choices | Latest `SystemReview`; owner-only and root-peer authenticated, no pkexec |
| Privileged helper `--system-review-json` | `ReviewDecision` JSON on stdin, at most `MAX_DECISION_BYTES` (64 KiB) | Latest `SystemReview`, or a nonzero failure |

Only the trusted OS helper may submit `system.review.decide`; the normal
frontend never calls that route directly or serializes capabilities.
The helper remains `/usr/local/bin/claw-approval-helper`, invoked through
`/usr/bin/pkexec`. The broker must independently validate the authenticated
owner, request id, displayed revision, allowed action and each permission
selection against protected state.
Cancellation uses the owner-only route above after the same displayed-revision
and empty-selection validation. It never invokes the privileged helper or falls
back to it on a transport error. Confirmation and apply actions still require
the trusted helper, and every outcome is followed by a root-broker refresh.

`ReviewDecision::decode` checks the 64 KiB byte limit before JSON parsing and
rejects unknown fields, including forged authority in nested selections.
It only decodes the wire shape: it performs no authorization, record lookup or
latest-revision check. The helper and bounded broker wrapper share this decoder;
the OS must still validate the decision against its current protected review.

`revision` identifies the displayed snapshot and must change when its decision
state, subject, permissions, supported choices or other review content changes.
Frontends bind events to the revision actually displayed, not the newest
revision fetched after a click. Competing or stale decisions must fail or return
current handled state. A revision or `contract_digest` is comparison data,
never proof of approval; installation still needs the OS's separate protected
confirmation/consumption path.

Draft choices start empty, not copied from current grants. Unselected entries
remain unchanged. Confirmation/cancellation requests cannot contain choices;
only `apply_choices` may submit explicit supported per-permission selections.
`restore` is labelled "Restore App policy": it applies the existing OS-managed
non-execution App-policy restoration through the trusted helper, not a new
restoration-request workflow or an Allow/Forever grant.
The current fixed-brokered deny/restore
controller should advertise only those choices it actually supports. Ask,
filesystem/network switches and other lifetime choices must remain absent
until their own controllers support them.
No client infers permission support from risk or offers Forever implicitly.
Unsupported choices require an explanation. Late-bound resources stay
"Selected/confirmed when used", not wildcard authority.

Map protected `approved` records to `confirmed` and `consumed` records to
`consumed`, not installation success. The consumed label says that confirmation
was used and the operation outcome is separate. Both states have no decision
actions; any installation or activation result remains the owning operation's
separate result.

Terminal formatting is read-only and escapes control/bidi text. Native review
presentation lives in
[`claw-applet-approval-gate`](../../desktop/applets/claw-applet-approval-gate/src/lib.rs)
and uses the same DTO and label keys, with matching English Fluent text.
Its per-request busy/error/stale/completed state is ephemeral presentation only.
Every decision outcome is followed by a root-authenticated `show`; failed,
ambiguous or missing requests are not silently treated as successful decisions.
No live daemon or user state is used by the synthetic presentation tests.

## Validation

From the OS repository root:

```bash
cargo test -p clawd-client --lib --locked
cargo clippy -p clawd-client --all-targets --locked -- -D warnings
cargo test --manifest-path desktop/applets/Cargo.toml \
  -p claw-applet-approval-gate --lib --locked
```

These checks cover DTO/label/model/transport and native widget construction,
not interactive Wayland, accessibility-device or full installed-system
acceptance. Backend route/helper/controller integration is separately owned;
JSON fixtures do not prove that its authorization or persistence is correct.
