# Activity monetary budgets

Activity monetary budgets are owner-defined accounting constraints for model
turns. They do not replace global AI consent, capabilities, approvals,
execution limits, provider configuration, or task cancellation. Terminal-only
and desktop installations use the same `clawd` broker and `activities.db`
backend; this increment intentionally adds no Web or native presentation.

## Accounting semantics

The only supported currency is `USD`. Every amount is an integer micro-USD
(one million micro-USD equals one USD). The configured input and output rates
are micro-USD per one million tokens. They are policy values selected by the
owner, not a built-in provider price catalogue and not provider invoice data.

Rates apply to provider-reported input and output tokens. Cache-read and
cache-write counts are retained with an actual settlement but are neither
discounted nor added a second time. Each component is rounded up independently:

```text
ceil(tokens * rate / 1,000,000)
```

The finite accepted ranges are:

| Field | Range |
| --- | --- |
| `max_total_microusd` | 1–1,000,000,000,000 |
| each rate | 1–1,000,000,000,000 micro-USD per million tokens |
| `max_output_tokens_per_turn` | 1–1,000,000 tokens |

## Configure and control

```bash
activity_id="00000000-0000-4000-8000-000000000001"

cos activity set-monetary-budget "$activity_id" \
  --currency USD \
  --max-total-microusd 5000000 \
  --input-microusd-per-million-tokens 250000 \
  --output-microusd-per-million-tokens 1000000 \
  --max-output-tokens-per-turn 4096

cos activity monetary-budget "$activity_id"
cos activity disable-monetary-budget "$activity_id" --revision 1
cos activity enable-monetary-budget "$activity_id" --revision 2
```

Creation omits `--revision`; updates supply the exact current revision.
Updates preserve `spent_microusd`, `reserved_microusd`, the ledger, and enabled
state. Creating, updating, and enabling require an active or paused Activity.
Reads and disabling remain available after completion or cancellation. Root
has no cross-owner exemption.

An Activity with no monetary budget retains legacy model behavior. Once a
budget exists, disabled, inactive, stale, or exhausted state blocks the model
turn before provider dispatch.

## Reservation and settlement

Immediately before every Activity model turn, the shared runtime creates a
random UUID and asks the authoritative Activity service to reserve:

```text
ceil(serialized-request-byte-bound * input-rate / 1,000,000)
+ ceil(configured-max-output-tokens * output-rate / 1,000,000)
```

The UTF-8 byte length of the provider-neutral outgoing request is used as a
conservative input-token upper bound. The runtime then clamps the actual
outgoing `max_tokens` to the configured output limit. Measuring before that
clamp remains conservative because the clamp can only keep or shorten the
serialized numeric value. The ledger binds the UUID to the exact owner,
Activity, Job, optional Session, turn index, request bounds, policy revision,
and rate snapshot. Replays are idempotent only when every bound identity field
matches.

For a successful call with nonzero provider usage and no observed retry or
fallback ambiguity, settlement releases the reservation and charges the actual
provider-reported input/output counts using the reservation's rate snapshot.
If actual cost exceeds the reservation, the full actual cost is recorded even
when that takes the Activity over its configured total; subsequent calls are
blocked.

Provider errors charge the full reservation. Missing usage, buffered retry
behavior configured for more than one attempt, or an observed provider-chain
switch also charges the full reservation because failed internal attempts may
have consumed billable tokens that the final `Usage` does not expose.
A single-attempt retry policy can still settle reported usage. Cancellation or a process crash never
silently refunds uncertain work: an explicit failure can settle the full
reservation, while a crash can leave durable `reserved_microusd` that continues
to reduce available budget.

This is deliberately an accounting upper bound, not provider-invoice accuracy.

## Broker contract

| Route | Request |
| --- | --- |
| `activity.monetary_budget.get` | Activity ID |
| `activity.monetary_budget.set` | Activity ID, optional expected revision, closed budget draft |
| `activity.monetary_budget.enabled` | Activity ID, expected revision, enabled flag |

Public requests cannot set owner, spent/reserved totals, ledger entries,
provider usage, or settlement state. The private worker channel accepts only
bounded reserve/settle exchanges for its one signed task; `clawd` re-derives
owner, Activity, Job, and Session from Root-owned state.

Database schema 6 adds the separate monetary policy and ledger without
rewriting Activity, receipt, object-state, execution-limit, or capability
policy rows.
