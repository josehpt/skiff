# Host API — M0 contract and seams

Tracking: [M0 #156](https://github.com/solutionscay/skiff/issues/156),
[epic #155](https://github.com/solutionscay/skiff/issues/155).
This is the repository copy of the proposed wiki contract page. It can be
published with the wiki glossary when the V2 docs are published; M0 does not
push to the separate wiki repository.

## Glossary

- **Host**: the machine/account that owns host-scoped data. Its opaque `HostId`
  is durable identity, never derived from a hostname, endpoint, socket or PID.
- **Endpoint**: a way to reach a Host. `Local { socket }` and
  `Ssh { hostname, account, port, socket }` are descriptors. The SSH variant has
  no implementation in M0. A Host may have more than one endpoint.
- **Selected Host**: the one Host whose data a future app window presents.
  Selection is not implemented in M0. No cross-Host aggregation is implied.
- **Capability**: `{ name, version }`, a feature's major contract. `sessions@1`
  is the only M0 operation family. Unknown names/majors are retained as metadata
  and ignored unless an operation requires them.

The ownership rule is: if data would differ on another Host, it must eventually
come from that Host. Projects/fleets/worktrees, Git, files/images, sessions/groups,
ledger/results, Skipper, task types and agents are Host-owned. Keys, appearance,
window state, local Open with policy and workspace-profile files are client-owned.
M0 does not move any of them.

## Code seams and current behavior

`skiff-host-api` depends at runtime only on serde and base64. It has no I/O,
application state, runtime, core re-exports, config reader or process launcher.

`skiff-client::transport::Transport` splits an ordered byte stream into owned,
Send/Unpin asynchronous reader/writer halves. Its production implementation is
UnixStream and delegates directly to the existing `into_split()`. The existing
`Client::connect` still connects to the same socket, reads the same local peer
credentials and sends only the existing skiffd requests. Ping/Pong, PROTOCOL 27,
input/output queues, lag recovery and terminal framing are unchanged.

`skiff-client::host::HostConnection` is an opt-in M0 interface over Transport.
It has no concrete socket or process-ownership type in its public API. It
negotiates Hello, gates typed requests, correlates replies, offers an event
receiver, and can disconnect. It is not called by the desktop, CLI or daemon.
A local UnixStream fixture and a fake duplex stream exercise the same interface.
No persistent IDs are fabricated and no identity/config file is written.

No `skiff-host` process, IPC proxy or extra listener is implemented. M1 decides
how the session/data planes are reached and introduces one HostConnection per
window; M2 introduces SSH. The proposed Host envelope below is a separate opt-in
contract, not a replacement accepted by today's skiffd socket.

## Schema, API major 1

Types in `crates/skiff-host-api/src/lib.rs` are the authoritative field list;
`tests/fixtures/m0.json` contains golden examples of every M0 variant.

- Host: `host_id` (opaque nonblank string), `os`, `account`.
- Endpoint: tagged `kind: local | ssh`, with the fields above. Paths are opaque
  strings on the named machine, never a command or an implicit local path.
- Hello: flattened Host fields, `instance_id` (opaque nonblank string),
  `service_versions` (name → version string), `api_major`, `capabilities`,
  `limits { max_frame_bytes, max_in_flight }`. Limits are positive integers;
  OS/account and capability names must be nonempty, capability versions nonzero,
  and duplicate capability/name-version pairs are malformed.
- Session: `id`, `cwd`, `label`, optional `generation`. No broad core SessionInfo
  dependency or new project/ledger schemas are introduced for future milestones.
- Envelope: `{ id, request }`; request is tagged `cmd` and is one of `hello`,
  `list_sessions`, `write { session, data, paste=false }`,
  `resize { session, cols, rows }`, `subscribe { session }`, `unsubscribe { session }`.
- Response: tagged `kind`: `hello { hello }`, `sessions { sessions }`, `ok`.
- Event: tagged `event`: `snapshot { session, data, cols, rows }`, `output { session, data }`,
  `session_changed { session }`. Snapshot dimensions and arbitrary bytes retain
  their session-plane meaning; this is not a new terminal implementation.
- Message: tagged `type`: `reply { id, result }` or `event { event }`. Result is
  exactly one of `{ "Ok": Response }` and `{ "Err": Error }`.

Control frames are newline-delimited UTF-8 JSON; each binary `data` is standard
base64, preserving arbitrary bytes including invalid UTF-8. Request IDs start
at 1 for a new connection, increase without reuse, and correlate replies even
when they arrive out of order. Events are ordered as received. Snapshot can
arrive before the subscribe reply: take the event receiver first.

All operation requests require exactly `sessions@1`; Hello is handshake-only.
A reply of the wrong variant is malformed. Unknown optional fields are ignored;
required fields and unknown message variants must parse or fail explicitly.
Future operations/messages must be gated behind a capability they require.

## Compatibility and errors

API major 1 changes only for an envelope/handshake break. A breaking feature
change increments that capability's major; serve the previous major for at
least one release line. Additive optional metadata needs no major bump. The
existing session-plane PROTOCOL and release/service version strings are separate
from both numbers. The supported product window is the current and previous
minor release lines; fixtures will accumulate as those Host releases exist.
Within the same API major, service versions alone are diagnostic: older or newer
services can offer reduced capabilities, never a fallback to local data.

Unknown API major is `incompatible_version { supported, received }` before any
operation dispatch. Missing/wrong capability major is `unsupported_capability
{ capability }`, also before dispatch. Error is tagged `code`; other variants:

- `malformed_contract { message }`, `not_found { message }`, `conflict { message }`,
  `permission { message }`;
- `host_busy`, `transport_lost`, `cancelled`, `timed_out`;
- `identity_changed { expected, received }` (HostIds).

These are contracts, not new UI strings. M1 maps them to existing design rules.
M0 does not change the legacy Client's errors or UI surfaces.

## Identity, cancellation, bounds and reconnect

Validate Hello before making the HostConnection available. On an endpoint's
next connection, pass the previous Hello: changed host id rejects reuse; changed
instance id returns NewInstance, requiring fresh subscriptions and snapshots.
SameInstance still creates fresh transport subscriptions. Host provisioning will
create durable host IDs and new instance IDs on start/reload; M0 only defines
and tests this lifecycle. There is no automatic reconnect/resubscription.

The local safety bounds are 64 in-flight requests, a 64-frame write queue,
8 MiB per frame (including newline) and 256 queued events per receiver. Outgoing
limits use the smaller of these and the advertised Host limits. The request
budget defaults to 20 seconds, including queueing; test callers can set it.
Oversize/incomplete/invalid incoming frames fail the connection explicitly.
Control replies are processed without waiting for event consumers. A slow event
receiver receives Lagged and must discard backlog and resubscribe for a snapshot.

Dropping a request future cancels its local wait/removes its correlation slot.
Canceled queued writes are skipped when possible; an already in-flight operation
cannot be retracted, so cancellation/timeout does not promise it did not run.
Late/duplicate replies have no waiter and are ignored. Disconnect completes all
remaining waiters once and closes event receivers/tasks. There is no replay of
uncertain requests or keystrokes, including after identity/instance changes.
Disconnecting affects only the connection, never host sessions/processes.

## Enforced import boundary

Run `python3 scripts/host-boundary/check.py` and the adjacent unittest suite.
CI runs both. The contract's runtime dependencies are checked against the exact
serde/base64 allowlist, and its source is checked for I/O/application references.

The desktop check catches Rust host imports (including aliases, grouped uses,
re-exports and fully qualified paths), filesystem/process/environment/network
access, and TS direct invoke/network/Node/Tauri I/O imports. Executable template
interpolations are scanned recursively, including nested templates. Literal
relative TS paths are normalized (dot segments, explicit extensions, emitted
`.js` paths and index barrels). Local adapter dependencies propagate transitively
through callers/re-exports; Rust module names and root/ancestor references are
conservatively tracked without symbol resolution. Imports of the new Host
contract/client seam are allowed.

The temporary baseline inventories 78 existing adapters/callers plus both desktop
dependency manifests with exact SHA256 content and purpose labels. New dependency
aliases/providers also require review. It is deliberately conservative: even
adding a call through an existing alias changes the whole file's fingerprint and
fails. Type-only imports, inert relative-path strings and ambiguous Rust names
may over-report. There are no directory or wildcard exemptions.
Client policy files (appearance, keys/UI/native menus, clipboard, workspace files)
have explicit exact-file entries rather than broad immunity.

Do not casually regenerate the baseline. Inspect a changed file's ownership,
then explicitly review any remaining direct local accesses and their purpose
before updating its fingerprint with `--record`. A file with no remaining direct
or transitive access must have its entry removed. M1–M6 progressively move providers into a
local adapter and shrink the inventory; graduation requires no legacy host-data
entries. This is a lexical architecture tripwire, not a malicious-code sandbox
or full Rust/TS semantic resolver: computed module specifiers, custom resolver
aliases, macro expansion and reflective/dynamically constructed I/O are not
resolved. Regex recognition inside template expressions uses a lexical heuristic.
Baseline and checker changes require review.

## M0 demo and handoff

M0 has zero visible behavior changes: desktop source, daemon source, protocol,
config/ledger paths and migrations are untouched. The only production connection
edit delegates the existing UnixStream split to the trait implementation that
performs the identical call. Test-only paired streams/private sockets prove the
old Ping JSON, protocol 27, ordered raw input and snapshot/output bytes still work.
Existing config/ledger fixture tests cover their unchanged on-disk contracts.

M1 still owns actual app routing, capability-driven surfaces, endpoint-local
lifecycle guards, persistence/Hello rollout and the session/data-plane topology.
M2–M6 own transport, services and data APIs. This document intentionally does not
implement SSH, a host process, host selection, migrations or data-location changes.

## Validation snapshot

Base: `03677f5` (v1.6.6 plus workspace fixes). M0 contract/local-parity tests,
the boundary check and negative fixtures, desktop tests/build, workspace compile
check and client/contract Clippy pass. After building the existing fixture
binaries, the workspace run passes 181 tests when excluding two pre-existing
macOS attention tests (`an_agent_title_keeps_a_quiet_turn_working` and
`an_agent_waits_at_its_prompt_and_its_turn_end_is_unread`). Both failures reproduce
from a clean `03677f5` source archive: fake agents are detected as bash. M0 does
not change foreground detection to hide this baseline failure. Linux-only suites
remain platform-gated on this Mac; no V2 Host service or installed app is launched.
