# Retained Package Transactions

This module coordinates generic Critical `sys.package` payload effects. It
does not provide another installer, App entitlement, trust root or SDK.
The Store's exact archive inspector, authenticated APT-source classifier,
publisher verifier, protected delivery reviews, tracked APT child and
pre-dpkg helper remain the package delivery boundary.

## Contract

- `packages::{install,control,restore}` accept payload work only after the
  authenticated peer owner and live session authorize the exact package
  capability. Upgrade-all requires wildcard authority; an install batch
  spends all exact named requirements together, once.
- Acceptance saves an owner/session/grant/invocation-bound Root record before
  starting an independent Root controller. `accepted` is not `applied`.
  The original ordinary/GUI provider and outer MCP call return normally;
  neither waits for the controller that may retire it. A cached exact
  invocation acknowledges its protected receipt without issuing another
  authorization proof or spending another use.
  Payload requests require a canonical invocation ID; the client creates it
  before dispatch. Missing IDs never fall back to an operation hash that
  could confuse a later independent action with an earlier receipt.
- Root package publication is serialized separately from APT provider work.
  The generic controller holds neither the APT mutex nor the dpkg database
  lock while waiting for App providers.
  Metadata providers already queued on that mutex can therefore drain.
  Directory publication and official Store publication share the publication
  reservation; database ownership is checked again after retirement.
- Catalog verification has its own private mutex inside `catalog()`.
  `package_catalog()` takes neither the Store serializer nor an APT lock.
  Catalog metadata/network I/O never enters APT, avoiding the Store
  serializer/publication/APT lock-order inversion.
- A concrete resolver plan binds versions, architectures, archives and
  removals. Official App-channel bytes require the existing signed inventory
  and every contained App's separate protected delivery confirmation.
  Critical package permission never substitutes for those confirmations.
- Plans changing the package that owns the active controller executable are
  refused before payload dispatch. Stopping `clawd` would kill its APT child;
  self-controller mutation needs an independently retained OS updater, not
  App-stop consent or an inferred completion after restart.
- Before payload effects, Root derives the concrete App-payload/package
  dependency impact and retires only that closure, across owners. The caller
  is included only when affected. Admission denial is scoped to the same set.
  Missing managers or unchecked legacy custody cannot authorize dpkg.
- Unbounded maintainer/trigger/APT-helper effects, ambiguous dependency
  providers and shared runtime access require a **separate protected owner
  all-App-stop confirmation**. The Root record binds this review to the exact
  APT plan, installed/retained App identities, impact evidence and uncertainty
  reasons. It uses its own `pg-` review and `confirm_global_app_stop` action
  through the existing authenticated OS helper/presenter. App-install reviews,
  Critical package permission, returned receipts and client booleans cannot
  approve it. Changed impact requires a fresh displayed review.
- Declared dependency users are evidence, not proof of exclusive shared-file
  readers. In particular the current common read-only `/usr` runtime and
  unmodelled Root helpers/triggers often prevent a narrower safe closure.
  These cases wait for the separate confirmation instead of silently stopping
  every App or claiming that maintainer effects were precisely derived.
- The existing `admit(app)` return type and owner admission gate are preserved.
  Its short global entry guard synchronizes pending checks and package-read
  acquisition; per-App writes drain full in-flight registrations. The publication
  reservation stabilizes the installed identity set, and retained/delivery IDs
  extend that closure. A global barrier is used only for the separately
  confirmed unbounded stop; bounded effects preserve unrelated admission.
  Persistent scoped denial remains across cancellation or unknown outcomes.
- The tracked Root helper checks APT v3 install/configure/removal actions,
  exact archive bytes, fresh authenticated sources, current App-channel
  inventory and one-use App delivery confirmations before dpkg.
- Root saves each mutation child's PID/start identity and normal-exit status.
  Only exit zero is successful completion. Timeout, failed or signal termination,
  missing completion or registry
  absence is not success. Installed package versions alone cannot replace
  child-completion evidence.
- Completion verifies the complete package/hold outcome, installed App
  signatures and the pre-dispatch App inventory. Unexpected App publication,
  removal or quarantine changes remain indeterminate.

`system.package.status` and `system.package.recover` derive the owner from the
authenticated peer, not request selectors. They expose only that owner's
records. Request/invocation IDs correlate records; they confer no authority.
`cos package-transactions [id]`, `--invocation <id>` and `--recover <id>` work
outside an affected App and do not require an Agent UI or model.

Recovery checks outcomes without rerunning APT. It cannot clear missing child
completion or interrupted checked cleanup merely because processes or local
registry rows disappeared. Such records retain admission denial and require
independent checked reconciliation. A stop before any cleanup/dispatch can be
reported as not-applied from the durable phase evidence.

Index update and hold/unhold remain synchronous metadata operations. A
payload-capable inverse always uses this controller, even when its concrete
plan needs only hold metadata. Session rollback stops at an accepted inverse,
keeps its uncertain marker and records it as rolled back only after an
authenticated Root completion; later inverses cannot overtake it.

## Source integration and checks

The common retained-binding inventory, protected system-review decision
channel and initialized custody managers are mandatory. Package impact consent
has its own kind/action and never substitutes for App delivery confirmation.
Unbounded work has no App-review/client-flag fallback.

Focused existing Rust coverage is selected with:

```text
cargo test -p cos --lib -- clawd::packages:: session::rollback::tests:: --test-threads=1
```

Existing metadata and `pkg` Python tests use synthetic public fixtures.
Syntax/metadata checks and these Python tests are not proof of actual Root
APT, ordinary/GUI/MCP self-retirement or installed-system acceptance. Linux
native/process fixtures remain required before publication.
