# Out-of-process Agent extension ABI

The Agent extension ABI is an observation and proposal boundary, not an
in-process plugin API and not an authorization-policy interface. Extension code
runs as a child of the existing per-task `claw-extension-host` isolation
domain. It never loads into `clawd` or `claw-agentd`.

## Activation

An extension activates only when all of the following are true:

1. Its id is explicitly listed in `agent.extensions`.
2. Its installed package passes
   [extension provenance verification](extension-provenance.md).
3. `extension.json` matches the signed package identity, version, content
   digest, and executable entry.
4. Its protocol/features, subscriptions, capabilities, and limits validate.
5. The real task-owned `claw-extension-host` completes `initialize -> ready`.

Selection is explicit in the ordinary per-user config:

```json
{
  "agent": {
    "extensions": ["example-observer"]
  }
}
```

The manifest schema is:

```json
{
  "schema_version": 1,
  "identity": {
    "id": "example-observer",
    "version": "1.0.0",
    "content_digest": "<digest of signed files other than extension.json>"
  },
  "entry": "bin/observer",
  "protocol": {
    "min_version": 2,
    "max_version": 2,
    "required_features": [
      "observational-events",
      "proposed-actions"
    ]
  },
  "subscriptions": [
    "session-start",
    "pre-model-call",
    "post-model-call",
    "pre-tool",
    "post-tool",
    "completion"
  ],
  "requested_capabilities": [
    {
      "verb": "sys.observe",
      "scope": { "kind": "name", "value": "time" }
    }
  ],
  "action_policies": [
    {
      "requested_index": 0,
      "tool": "now",
      "policy_id": "builtin.now/v1"
    }
  ],
  "limits": {
    "event_timeout_ms": 1000,
    "queue_capacity": 8,
    "max_output_bytes": 4096,
    "max_actions_per_event": 2,
    "max_in_flight": 1
  }
}
```

Identity and version must match the authenticated `.provenance.json` envelope
(or the root-owned vendor package metadata). The content digest covers all
verified payload files except `extension.json`, avoiding a self-referential
digest while still binding the executable and assets. The entry is a signed
executable file. ABI v2 permits 50–5000 ms event deadlines, 1–32 queued events,
1–8192 bytes of output, at most four proposed actions, and exactly one
in-flight event per extension.

## Process and authority boundary

The host reuses the complete
[`claw-extension-host` isolation design](extension-host-isolation.md):
exclusive package-created uid, non-broker gid, `NoNewPrivs`, non-dumpability,
seccomp, finite rlimits, mandatory cgroup-v2 CPU/memory/PID bounds, private
mount/PID/network namespaces, empty root, private writable state, and verified
tree cleanup.

Before the host drops to its dedicated uid, `clawd` verifies configured
packages with the task owner's trust roots and binds exact package receipts
into the private host bootstrap. The host cannot read the owner's mode-0600
trust files; it uses that broker-authenticated receipt to independently
re-open the installed path, verify the embedded publisher signature and
complete file tree through `crate::provenance::verify`, and materialize only
the resulting `VerifiedPackage` snapshot under task-private storage. A receipt
names the exact kind, id, version, content digest, signer-key digest, trust
tier, and trust generation. No package bytes or worker-selected trust roots
cross the worker control protocol. Only the authenticated package snapshot,
exact system interpreter/ELF dependency closure, minimal account files, and
private state enter the child. Agent extension children do **not** receive the
private extension broker socket, the routed session registry, the primary
broker, credentials, provider state, owner home, live package path, or ambient
network.

Every ABI binding includes the exact owner uid, task id, session id, extension
id/version, package/manifest/entry digests, capability generation, host lease
digest, and a 256-bit instance nonce. Every response must echo the complete
binding and sequence. Cross-session or stale-instance substitution disables
only that extension.

## Framing and lifecycle

Stdin/stdout carry typed JSON frames:

```text
4 bytes  magic "CEX1"
1 byte   kind: 1 host request, 2 extension response
1 byte   reserved, must be zero
4 bytes  unsigned big-endian JSON length
N bytes  UTF-8 JSON
```

The maximum frame is 65,536 bytes and the maximum event projection is 16 KiB.
Length is checked before allocation. A short, malformed, wrong-kind, oversized,
uncorrelated, or wrong-version frame fails closed.

The state machine is exact:

```text
initialize -> ready -> (event -> result)* -> shutdown -> shutdown-ack
```

`ready` selects the negotiated protocol and accepts required features. An
extension cannot select below the manifest minimum, above either maximum, or a
version other than the host's current ABI. There is no legacy parse or
downgrade fallback.

Current ABI version: **2**.

Compatibility policy:

- New optional fields are additive and ignored by older readers.
- New required behavior is named in `required_features`; an unknown or
  unaccepted required feature rejects activation.
- New lifecycle variants, changed field meanings, removals, and type changes
  require a new protocol version.
- When multiple versions exist, negotiation selects the highest mutually
  supported version that satisfies every required feature. Silent downgrade is
  always rejected.
- The worker-to-host control protocol is separately versioned and replaced in
  lockstep with the package. Agentd worker protocol v10 and signed grant format
  v9 carry the authenticated package receipts; extension-host control remains
  v8 and this child ABI remains v2.

## Observational events

Events never block or mutate the canonical model/tool path. Each extension has
one ordered queue, its own worker, and event-slot accounting that reserves a
terminal slot. Runtime observers use bounded `try_send`; a full queue drops
that extension's event and emits an audit record. Eight consecutive drops
stop new ingress and enqueue a terminal disable behind every already accepted
event; the accepted FIFO is still processed in order. Trust revocation or
protocol compromise instead discards queued observations immediately.
Completion uses the same FIFO, is sent only when subscribed, and cannot
overtake an earlier event.

Host admission uses independent bounded lanes for canonical App/MCP work,
Agent events, and priority detach/revocation/shutdown. At most one event per
extension is in flight, aggregate event capacity covers all 64 extension
slots, and excess work receives a correlated typed `busy` response. Short
pre-authentication/read budgets and one global task/FD ceiling bound stalled
connections without lending canonical or priority permits to event work.

Task finish, completion acknowledgement, detach retries, worker abort, and
forced cleanup share one total deadline. Detach acknowledgement is independent
of worker completion: a failed detach is retried even after its FIFO worker
has exited. If the host cannot prove exact child termination, the worker
returns a terminal failure and requests host shutdown so `clawd` performs the
mandatory cgroup kill/empty/quarantine cleanup. A crash, hang, malformed
result, or limit violation normally has per-extension failure scope; inability
to prove containment escalates to the task host.

Event projections are least-privilege:

| Event | Projection |
| --- | --- |
| `session-start` | trusted source class, attended/delegated booleans |
| `pre-model-call` | turn index, attempt id, and provider/model identities |
| `post-model-call` | matching attempt id, provider/model, model-only latency, token counts, stable error class |
| `pre-tool` | tool identity, call-id digest, input byte count/digest |
| `post-tool` | tool identity, success, latency, result byte count/digest |
| `completion` | success, turn count, answer byte count/digest |

Prompts, messages, tool arguments/results, reasoning state, credentials, secret
values, and answer text do not cross ABI v2. Model events are emitted at the
actual provider-attempt boundary: retries and fallbacks receive distinct paired
ids, `post-model-call` occurs before any tool dispatch, and turns that never
invoke a provider emit no model event. Extension output is bounded,
treated as untrusted, represented by a keyed digest in audit/session mutation
records, and never inserted into system/developer prompts or the conversation.
A future model-visible surface must use a fixed trusted local tool-result
envelope and the normal untrusted-data wrapper.

The worker retains the authenticated `VerifiedPackage` snapshot for the active
instance and rechecks it before every event, immediately before every proposed
action, and at least every five seconds while idle. Trust-generation changes,
package replacement, or revocation disable and detach only that extension; a
running task never continues to invoke code whose package is no longer current.

## Proposed actions and capability references

An extension result may contain explicit proposed actions. It cannot execute an
action itself and cannot synchronously answer an authorization question.

For each event the worker creates one absolute Linux monotonic deadline and
transmits it unchanged through worker-host control and CEX1. The same deadline
expires connection admission, descendant discovery, child write/read, frame
validation, response processing, and the event's 256-bit opaque capability
references. `/proc` discovery runs as immutable blocking work under that
deadline; a late result cannot update lifecycle state or retain an event slot.
Each active extension
has an independent store sized from its authenticated action-policy count; no
extension can borrow another's quota. Every reference is bound at mint time to
owner, session, task, extension, manifest digest, capability generation, event
id, exact capability, requested index, allowed tool, and versioned policy id.
The store keeps only SHA-256 handle keys. Guessing, replay, cross-event,
cross-session, wrong-index, tool/policy substitution, exact-scope mismatch, and
expired references return the same denial. Raw credentials and secret values
are never references and never cross the ABI.

For an accepted proposal, the worker:

1. requires an authenticated manifest action policy naming the tool, requested
   capability index, and versioned tool policy id;
2. asks the registered tool to validate/canonicalize the input and derive one
   exact capability before exposure or approval;
3. rejects the entire result unless every proposed action is cooperatively
   cancellable, policy-identical, and exact-capability-identical;
4. atomically consumes all result references before executing any action; an
   invalid item retires the complete event lease so no valid prefix executes;
5. installs a task-local capability ceiling containing only the derived
   capability and runs normal exposure, guardrail, operation-bound capability
   approval, provider enforcement, and audit;
6. runs the preauthorized tool in an abortable task with a 30-second action
   deadline and periodic package-revocation checks;
7. records only bounded result metadata/digests and does not return the tool
   result to the extension or inject it into the model trajectory.

Proposal support is default-deny and independent of ordinary tool exposure.
`cos_delegate`, provider/model selectors, credential tools, shell/process
primitives, MCP gateways, legacy proxies, and every blocking/non-cooperative
tool are categorically non-proposable. ABI v2 initially enables only the
side-effect-free `now` tool under `builtin.now/v1`, deriving exactly
`sys.observe` on `name:time`.
Extensions cannot mutate the canonical system prompt, grants, approvals,
authorization rules, or audit history.

## Failure and shutdown

Initialize has a five-second deadline; events use one unchanged monotonic
deadline; shutdown has two seconds. Host control timeouts are longer than child deadlines,
so a hung child is killed without cancelling the whole host. The host tracks
the sandbox process tree, kills and reaps adopted descendants, and verifies no
known process identity survived before reporting detach success. Final task
cleanup still applies `cgroup.kill` and proves the complete containment group
empty before releasing the extension uid.

Hostile coverage lives in:

- `core/test/unit/agent_extensions/`
- `core/test/unit/extension_host/abi.rs`
- `core/tests/extension_provenance_process.rs`
- `core/tests/extension_host_boundary.rs`
