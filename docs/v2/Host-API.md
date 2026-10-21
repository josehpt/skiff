# Host API — M0 contract and local M1a/M1b/M1c seams

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
  is the M0 operation family; M1a adds `connection_status@1` and M1b adds
  read-only `session_screen@1`; M1c adds `session_inventory@1`. Unknown names/majors are retained as metadata
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
receiver, and can disconnect. The CLI and daemon do not use it. M1a integrates desktop connection status, M1b adds a text snapshot, and M1c
adds the desktop inventory read through the same local adapter, as described below.
A local UnixStream fixture and a fake duplex stream exercise the same interface.
M0 fixtures wrote no identity/config file; M1a provisions local bootstrap
identity as described below, without changing the legacy daemon protocol.

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

M0 session operation requests require exactly `sessions@1`; Hello is handshake-only.
M1a `connection_status` requires exactly `connection_status@1` and replies with
`{ kind: "connection_status", version, protocol }`; these are the existing skiffd
service version and session-plane PROTOCOL, not Host API versions.
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

## M1a: selected local Host owns connection status

The desktop's current single-window `App` runtime owns one fixed `LocalSelection`
and one active `SelectedLocalHost`. Only the selection can create its pending
client, and only that pending client can activate the Host. The accepted legacy
`Arc<Client>` is the exact same allocation lent to existing terminal/data callers
and the nonblocking input path; there is no second socket or terminal pipeline.
Initial connect, restart probe and reload reconnect obtain clients from this
selection. Existing compatibility, spawn/reload, CLI and watcher policy is kept.
This is **not** full M1 endpoint-policy/capability adoption.

The vertical slice is the existing `daemon_status` Ping/Pong refresh. It now
requests `connection_status@1` through `HostConnection`; the in-process local
`StatusSource` delegates to `Client::hello()` on that same selected client. No
Host envelope or Hello is sent to legacy skiffd. No listener, proxy, Host service,
worker process or extra round trip is added. The command's JSON shape, timeout,
warning logic and visible GUI flow stay unchanged. The broad session plane and event
streams are deliberately not advertised by this adapter. M1b below adds one
independent read; other session calls still use the legacy client. Missing/wrong capability major and unknown API
major are typed errors before source dispatch, never fallback to another client.
Local status requests have a 20-second limit, at most 64 in flight, cancellation
by dropping the future, and disconnect wakes pending calls without replay.

Local bootstrap uses an opaque random UUID in `local-host-id` beside the already
selected `projects.toml`. This is additive identity metadata, not daemon settings;
no existing config, state or ledger bytes move or change. Existing private-state
selection and `SKIFF_CONFIG` apply, so test/debug identities stay isolated. File
creation uses private `0600` permissions, a complete synced temporary file and
atomic no-clobber publication so concurrent app launches agree. Existing corrupt,
public, symlink or foreign-owned identity files fail explicitly, without silently
replacing identity or falling back to a path/PID-derived value. Bootstrap I/O runs
off async workers. It happens only after existing daemon compatibility policy
accepts a connection. An identity change within this selection rejects activation.

The UUID is durable for this local bootstrap store. `instance_id` is a fresh
**adapter connection generation**, not an authenticated daemon identity or PID;
reconnect conservatively yields NewInstance. Metadata advertises
`local-status-adapter` as its service, never a nonexistent `skiff-host`. Authoritative
daemon/service Hello rollout remains a later step; the future Host service must
adopt this identity deliberately. Copying identity metadata to another machine
is not supported provisioning or authentication.

Tests exercise the real desktop status entry point against a private fake daemon,
assert exact Ping IDs and shared Client identity, retain config/ledger bytes, and
verify reuse without another connection. Separate fixtures prove only the selected
endpoint receives status traffic, durable identity/concurrent bootstrap, explicit
capability/version/identity failures, cancellation/bounds and unchanged terminal
wire bytes. `local_host` is included in the existing boundary CI job.

Next deliberately excluded step: move another operation family (and its capability
checks) through the selected connection, with endpoint-local lifecycle guards and
authoritative Hello rollout designed before any remote endpoint. No SSH, host
picker, skiff-host service, migrations, data relocation or broad data conversion is
part of M1a. Per-surface capability UI and session event routing remain excluded.

## M1b: one read-only session text snapshot

The chosen operation is the existing Mission control `session_screen` command,
not session inventory. Inventory would require expanding the M0 session projection
to preserve the GUI's full SessionInfo; the text snapshot already returns only
`Vec<String>`. No UI source, screen rendering, command arguments or JSON result
shape changes. The desktop command calls `connection::screen_now`, obtains the
same selected owner via `ensure_host`, and requests the snapshot through that
owner's HostConnection. Its status and screen sources capture the very same
legacy `Arc<Client>`, created only by the fixed LocalSelection.

Additive API-major-1 contract:

- Capability: `session_screen@1`, independent of broad `sessions@1`.
- Request: `{ cmd: "session_screen", session, rows }` (`rows: u16`).
- Reply: `{ kind: "session_screen", lines: string[] }`.
- `operation_failed { message }`: an opaque legacy service rejection, with no
  invented not-found/permission classification. This error is only added for
  the new capability. Existing M0/M1a shapes/versions remain unchanged.

The in-process source sends exactly the existing `{ id, cmd: "screen", session,
rows }` to skiffd on the selected Client. The daemon's existing text extraction,
line order, empty lines, Unicode and whitespace are passed through unmodified;
zero and maximum u16 row counts are forwarded without client-side clamping.
No Host envelope, extra handshake, subscription, connection or request is added
to skiffd's wire protocol. Session inventory remains on its existing path.

Version/capability validation occurs before source dispatch; unknown API major,
missing capability and wrong capability major fail explicitly, without fallback.
The local adapter advertises only status plus this read-only capability, never
`sessions@1`. Mutations/lifecycle and event subscriptions stay unsupported by the
local Host contract. Both the local fake and negotiated transport test the new
request/reply family.

The temporary `legacy_screen` compatibility projection gives the existing Tauri
caller exactly its old error text: service messages (including older-daemon
"unknown variant" replies), unexpected reply diagnostics, connection closed and
the existing 20-second timeout text. Internally, stalls are typed TimedOut,
disconnect is TransportLost, malformed replies are MalformedContract and opaque
service rejections are OperationFailed. It never retries via a raw Client.

Mission control fans out reads for every visible tile. Snapshot requests wait
for a Host slot within their existing 20-second budget rather than acquiring a
slot with a new HostBusy failure on boards above 64 tiles. Drop/cancellation
releases a slot; disconnect wakes active and queued reads without replay. One
slot is reserved for status on the production 64-slot connection so a full board
cannot exhaust the status refresh quota. M1a status admission behavior is
retained. Raw terminal I/O and subscriptions
remain untouched.

Evidence: golden M1b types, fake gating/error/cancellation/deadline and 65-read
fanout fixtures, negotiated-transport coverage, and a private selected-socket
fixture comparing legacy and Host requests/results/errors on the exact same
Client. The desktop entry-point fixture checks both returned text and the
older-daemon error on the same accepted status connection. Existing M0/M1a and
bootstrap regression fixtures stay enabled in CI.

M1b deliberately deferred inventory until a lossless GUI metadata schema was
specified; M1c below supplies it. Terminal snapshots/events, mutation/lifecycle
and endpoint lifecycle policy still require their own increments. No SSH/remote transport, picker, service,
projects/files/ledger/Git movement, migrations or broad GUI conversion is part
of M1b. Bootstrap identity remains local adapter identity as described in M1a;
this slice does not turn legacy skiffd into an authoritative Host service.

## M1c: lossless desktop session inventory read

Only the existing Tauri `list_sessions` read now calls `inventory_now`, then
`legacy_inventory` on the same selected local HostConnection. Internal daemon
compatibility/reload checks still use their original legacy Client calls, avoiding
any bootstrap dependency cycle. No UI source, session mutation, event subscription
or other command path is converted.

Additive API-major-1 capability `session_inventory@1` has request
`{ cmd: "session_inventory" }` and response `{ kind: "session_inventory", sessions }`.
It is independent of `sessions@1`; M0 ListSessions/Session remains unchanged, and
local mutation/subscription operations are still unsupported. An InventorySource
on the same LegacyReadOnly/Arc<Client> sends exactly one existing
`{ id, cmd: "list_sessions" }` frame to skiffd. It neither chooses another endpoint
nor falls back to raw local access when capability/version validation fails.

`skiff-host-api::inventory::InventorySession` is deliberately only a read view of
today's desktop SessionInfo. It owns primitive/nested types, has no core re-export
or dependency, no SessionSpec, and no I/O. Its complete current field list is:

- Identity/display/program: `id`, `label`, `role`, `cwd`, `command`,
  `foreground_program`, `args`, `name`, `title`, `theme`.
- Observed session state: `state`, `cols`, `rows`, `pid`, `exit_code`,
  `started_at`, `last_output_at`, `unread`, `agent_exit`, `peek`, `interactive`,
  `input_gen`, `coordinator`.
- Existing read metadata: `away` (`path, command, at`), `was`/`resume`
  (`agent, title`), `watch` (`since, pids, failure, follows`), with watch failure
  (`kind, message, at`). These are metadata already in the session list, not
  project, filesystem, ledger or watcher-service APIs.

Role/state enums retain their existing four lowercase variants each. Integer
widths/signedness, vector ordering and duplicates, empty strings, zero values and
all 27 fields are preserved. Paths are opaque UTF-8 host strings with no filesystem
lookup, normalization or lossy conversion. Paths decoded from the legacy JSON wire
are UTF-8; an out-of-domain non-UTF-8 core path is explicitly rejected.

Serde defaults and null/omission rules match legacy output: false coordinator and
absent watch are omitted; nullable display/history fields remain null; missing
old-daemon counters/flags/lists get the same defaults; watch follows None means
unknown (omitted), while Some(empty) remains an empty array. Unknown source fields
are ignored by the existing core decoder as before; the view is lossless for the
current desktop model, not an arbitrary future JSON extension bag.

The adapter maps core → API → core explicitly, retaining the existing Tauri
`Vec<SessionInfo>` result and JSON shape. Struct destructuring/initializers and
enum matches are exhaustive, without `..`/wildcard fallbacks, so added core/API
fields or variants require deliberate projection review at compile time. Golden
fixtures plus legacy-vs-Host JSON equality cover full metadata, minimal old-daemon
data, all role/state combinations, extrema and nested optional semantics.

The operation shares the bounded read lane/20-second budget with screens, waits
rather than producing new HostBusy failures, and retains the status reservation.
Capability/version rejection happens before dispatch. Decode provenance is retained
through request_decoded: correlated malformed rows/nested data are MalformedContract,
genuine daemon Error is OperationFailed even with identical diagnostic text, and
stalls/disconnect are TimedOut/TransportLost. The compatibility helper preserves
legacy error text. No retry, input or request replay is introduced.

Tests include fake validation/ordering/metadata/deadline/cancellation, negotiated
Host transport, and a real selected-socket comparison of exact legacy frames,
ordering, duplicates, all fields and serialized output. They also cover malformed
inventory, genuine service errors with decode-like messages, a withheld reply,
malformed reply followed by EOF and an unused second endpoint. The real desktop
read entry point verifies the full result and service error on the same connection
as status/screen. Existing M0/M1a/M1b regressions remain in the boundary CI job.

Next excluded boundary: live session events/subscriptions still use the legacy
stream and need their own scoped contract/continuity work; inventory adoption does
not authorize mutation/lifecycle actions. Endpoint lifecycle guards, authoritative
Host Hello, SSH/remote selection/service, data conversion and migrations remain
outside M1c. Local bootstrap/adapter-generation identity limitations are unchanged.

## M0 demo and handoff

M0 has zero visible behavior changes: desktop source, daemon source, protocol,
config/ledger paths and migrations are untouched. The only production connection
edit delegates the existing UnixStream split to the trait implementation that
performs the identical call. Test-only paired streams/private sockets prove the
old Ping JSON, protocol 27, ordered raw input and snapshot/output bytes still work.
Existing config/ledger fixture tests cover their unchanged on-disk contracts.

Remaining M1 work includes routing beyond these read seams, capability-driven surfaces, endpoint-local
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
