# Host API — M0 contract and local M1a–M1g seams

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
  read-only `session_screen@1`; M1c adds `session_inventory@1`; M1d adds read-only
  `session_updates@1` events; M1e adds `session_terminal@1` (attach, output,
  input, resize, detach of existing sessions); M1g adds `session_lifecycle@1`
  (create, kill, rename, theme, seen) and `session_groups@1` (list, save, delete).
  Unknown names/majors are retained as metadata and ignored unless an operation
  requires them.

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

The temporary baseline inventories 77 existing adapters/callers plus both desktop
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
reconnect conservatively yields NewInstance. (M1f: skiffd now serves this
identity itself; the bootstrap path remains only for older daemons.) Metadata advertises
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

## M1d: one read-only session-update stream

The chosen path is the existing Tauri `subscribe_events` stream: every non-output
skiffd event (state, away, title, exit, created, turn ended, updated, removed,
groups changed) that the page applies to its session list. It now calls
`updates_now`, which obtains the same selected owner via `ensure_host` and takes
`legacy_updates` from that owner's HostConnection. Terminal output/snapshots,
`subscribe_output`, input and every mutation/lifecycle path stay on the legacy
Client. The Tauri command name, argument, channel JSON, task replacement and the
page's own resubscribe/refresh policy are unchanged; no UI source changes.

Additive API-major-1 contract, explicit at each layer:

- Capability: `session_updates@1`, independent of `sessions@1`. It grants no
  request, output, input or lifecycle operation.
- Envelope: the existing `Message::Event`; new `Event` variant
  `{ event: "session_update", update }`. `Event::capability()` names the family's
  capability (`session_updates@1` here, `sessions@1` for the M0 events).
- Payload `SessionUpdate`, tagged `kind`: `state { session, state }`,
  `away { session, away[] }`, `title { session, title|null }`,
  `exit { session, code|null }`, `session_created { session }`,
  `turn_ended { session }`, `session_updated { session }`,
  `session_removed { session }`, `groups_changed`. Created/updated rows reuse the
  lossless M1c `InventorySession`; state/away reuse its enums/paths.
- No subscribe/unsubscribe request, acknowledgement or sequence number: a Host
  pushes the family to each connection that negotiated it, as skiffd does today.

`HostConnection::session_updates()` checks the capability first on both backends
and never falls back; an adapter without an `UpdateSource` cannot advertise it.
The local source wraps a receiver of the very same `Client::events()` broadcast the
desktop read directly before, created synchronously when the command runs, so the
page's "events first, then list" ordering holds. No frame is sent to skiffd. The
Client still routes output elsewhere and skips unreadable/unknown frames, so they
neither reach nor end the stream. Core → API → core mapping is exhaustive (no
wildcards); output/snapshot can never be projected as an update.

Receivers report `UpdateGap::Lagged(n)` (nothing replayed), `Malformed` (one
unprojectable event skipped, e.g. a non-UTF-8 path, unreachable from the JSON wire)
and `Closed`. The desktop compatibility receiver skips the first two exactly as the
old loop ignored Lagged, and ends on Closed. Local streams close on `disconnect()`
or when the connection is dropped, even with updates queued. Before M1d a dead
connection's task idled until the page replaced it; now it ends when reconnect,
restart or reload drops the selected owner. In both cases nothing more could
arrive and the page subscribes again as before. On the wire backend, malformed
frames fail the connection (existing rule) and the stream closes; other event
families are skipped by this receiver.

Tests: golden `m1d.json` (every kind, nulls/empties, rich and minimal rows,
malformed kinds/states), fake capability/source gating, ordering, gap reporting,
disconnect/drop closure, negotiated-transport gating/filtering/closure, and a
selected private socket comparing the legacy receiver and the Host stream on one
Client with interleaved output and unreadable frames and no outgoing frame. The
desktop entry-point fixture checks `updates_now` on the same status/screen/inventory
connection and its closure when the owner is dropped.

Next excluded boundary: terminal output/snapshot streaming (`subscribe_output`,
lag/resubscribe flow control; moved in M1e below) and all mutation/lifecycle
actions remain on the legacy Client and need their own scoped contracts. No SSH/remote transport,
picker, Host service, authoritative Hello, data migration or projects/files/
ledger/Git conversion is part of M1d.

## M1e: the session terminal through the selected Host

The existing desktop terminal path now reaches skiffd only through the selected
local `HostConnection`: `subscribe_output` (attach, snapshot, output, lag and
resubscribe), `unsubscribe_output`, `pty_resize`, and every keystroke/paste from
`pty_write` via the input bridge, including held input released after reload
or first connect. `App::live` now holds the selected HostConnection, not a raw
Client. The streaming loop, flow control (`BEHIND_HIGH`/`BEHIND_LOW`, acks,
64 KiB pieces), page channel bytes, echo JSON for the latency trace, Tauri
command names/arguments and UI source are unchanged. Session lifecycle and
mutations (create, kill, rename, theme, seen, coordinator), Skipper, config,
ledger and Git remain on the legacy Client.

Additive API-major-1 contract (`skiff-host-api::terminal`):

- Capability `session_terminal@1`, independent of `sessions@1`; it grants no
  create/kill/rename or inventory. M0 `write`/`resize`/`subscribe`/`unsubscribe`
  and `snapshot`/`output` stay `sessions@1` and unchanged.
- Requests, each answered `ok`: `terminal_attach { session }` (subscribe; a
  snapshot precedes later output and can arrive before the reply),
  `terminal_detach { session }`, `terminal_resize { session, cols, rows }`,
  `terminal_input { session, data, paste=false }` (base64 bytes).
- Events: `terminal_snapshot { session, data, cols, rows }` and
  `terminal_output { session, data, echo? }`; `echo { wrote, read }` is optional
  diagnostic metadata. `Event::into_terminal()` yields `TerminalFrame`.
- `Endpoint::is_local()` names the only endpoint kind the app may manage.

`HostConnection::terminal_output(session)` returns a receiver; take it before
attach. `terminal_input` is synchronous: it queues without waiting for a reply,
so calls from one thread keep order, and an error means this input was not
sent. Both check the capability first and never fall back to a raw Client.
`TerminalOutput::recv` reports `OutputGap::Lagged` (frames dropped: clear,
attach again, skip output until the new snapshot; nothing replayed) and
`OutputGap::Closed`. `HostConnection::is_closed()` is a no-round-trip liveness
check; the local adapter asks its source.

Local adapter: `LegacyTerminal` over the same selected `Arc<Client>` sends
exactly the legacy frames (`subscribe`, `unsubscribe`, `resize`, `write`, ids
from the same counter) and uses the existing per-session output queue (4096
chunks, lag flag, receiver replaced per session, closed on unsubscribe/EOF).
Input keeps the unbounded legacy queue so a keystroke never waits. Attach,
detach and resize have their own 64-slot lane within the 20-second budget, apart
from reads and the status reservation. Local receivers also close when the
selected owner is dropped (reconnect, restart, reload). Legacy error text is
kept by `legacy_attach/detach/resize/input`.

Wire backend: attach/detach/resize are ordinary correlated requests. Input is
written in call order on the bounded 64-frame write queue, is not cancellable
and has no waiter; a full queue refuses it with `host_busy` rather than queueing
for retry. Terminal events share the 256-event receiver bound; a slow receiver
reports Lagged. Whether input on a slow remote link should block, drop or turn
input off is an M2 decision (epic §4: input off while disconnected).

Reconnect and no replay: the desktop's existing policy is unchanged. Input typed
while a reload runs or before a connection exists is held (never sent, so never
uncertain) and released once onto the new connection only for sessions proven
to be the same child; anything already queued on a lost connection is gone, and
nothing is resent. After a lost connection, the page resubscribes and draws from
a fresh snapshot. Local continuity is always conservatively NewInstance (see
below), so every reconnect takes fresh subscriptions and snapshots.

Endpoint-local lifecycle guards: `ProcessControl` can only be made from a local
`Endpoint` (`LocalSelection::process_control`, `SelectedLocalHost::process_control`).
Daemon spawn in connect, SIGTERM/SIGKILL in compatibility replacement and in
restart, and the reload path now require it; an SSH descriptor never yields one.
CLI link reconciliation (`cli_path::sync`) and Skipper watcher reconciliation
(`skiff_agent::ensure_watch`) are not reached by the terminal boundary; M1f
below guards them.

Authoritative identity prerequisite: the local Hello is still synthesized by the
adapter (bootstrap host id file, adapter-generation instance id). skiffd has no
authoritative instance/generation in its Ping reply, so SameInstance can never be
proven locally, and a forwarded socket's peer credentials must never stand in for
it. Before any remote terminal (M2), a Host service or skiffd must send a real
Hello with a durable host id and a start/reload generation; this slice does not
fabricate one. M1f below supplies it from skiffd.

Tests: golden `m1e.json` (requests, events, echo extrema, legacy frame map),
fake capability/source gating, ordering, lag/clear, disconnect/drop/lost-source
closure and no replay; negotiated-transport gating, session filtering, input
order without waiting, ignored input replies, lag and disconnect; a selected
private socket comparing exact legacy frames for attach/input/paste/resize/
detach on one Client, a 5000-chunk flood reporting lag once, clear and EOF;
the desktop entry point for terminal attach/output/echo/input/resize/detach on
the same status/screen/inventory/updates connection; and the existing real
skiffd reload fixture (`held_input_survives_a_reload`) now typing through the
Host input path.

## M1f: service-provided Host identity and the remaining lifecycle guards

### Topology decision

The epic leaves open how the app reaches the session plane (skiffd) and the
future data plane (`skiff-host`): one socket where `skiff-host` proxies to
skiffd, or one SSH session carrying a forward to each. M1 decides for the
session plane now. **skiffd serves its own Host identity, and the app reaches
it directly; there is no proxy, Host process, listener or wrapper.** When a
data-plane service exists, it is a second endpoint of the same Host. It must
report the same `host_id` from the same store, and the app's one
HostConnection checks that both agree. It has its own `instance_id`, because it
restarts on its own schedule.

Tradeoff:
- A proxy would put a second process on every keystroke and output byte, the
  latency-critical path that #10/#12/#15 measure.
- With a proxy, a restart of the data plane would drop every terminal
  connection, which is the #62 failure the two-process split exists to avoid.
- The cost is two forwards and two handshakes per remote Host in M2, and a
  rule that their host ids must match.
- This was decided structurally, not measured. No data-plane service exists to
  put behind a proxy, and the direct path is the one already measured in
  production. M2 should still take RTT numbers over a real forward before the
  data plane lands.

### Authoritative identity

- `pong` gains one optional field, `identity { host_id, instance_id, os, account }`:
  - an older client ignores it
  - an older daemon omits it
  - `PROTOCOL` stays 27, under its documented rule for optional fields
  - the `pong` JSON of a daemon without it is unchanged
- **Durable id.** skiffd reads `host_id` from `local-host-id` beside its
  selected `projects.toml` (`skiff_core::host_identity`):
  - This is the same file, with the same private `0600`, no-clobber, no-symlink
    and refuse-to-replace rules, that the M1a desktop bootstrap wrote. The rules
    moved from `skiff-client` into `skiff-core`.
  - The Host now owns the id an existing install already has. Nothing is
    regenerated or migrated.
  - Private test/debug state keeps its own id, because the path follows the
    socket's private state and `SKIFF_CONFIG`.
- **Instance id.** `instance_id` is a random UUID made once per process image,
  after the socket is known. A reload execs a new image, so every start and
  every reload is a new instance, while the pid and sessions stay. It is never
  a pid, socket path or peer credential.
- **`os` and `account`** are skiffd's own (`uid:<euid>` of the daemon). A remote
  Host reports its own account, not the app's.
- **If skiffd cannot read its id** (corrupt or foreign file), it logs, omits
  `identity` and keeps serving sessions.

### Adapter and reconnect

- `PendingLocalConnection::daemon_info()` records the `pong` it reads.
  `activate()` sends nothing itself and adopts that pong's identity:
  `IdentitySource::Host`, with `service_versions` naming `skiffd`. It does not
  read the app's bootstrap file.
- The desktop's existing pre-activation Ping is that recorded `pong`, both on
  connect (`ensure_host`, then `check_info`) and on reload reconnect, so
  activation costs no extra round trip.
- **Older skiffd.** When there is no identity, or no recorded `pong`, activation
  uses the M1a bootstrap (`IdentitySource::LocalBootstrap`). Its adapter
  generation makes every reconnect NewInstance. After a reload onto an M1f
  skiffd, the same store yields the same `host_id` and continuity is
  NewInstance.
- **Validation on each activation:**
  - a changed `host_id` is `identity_changed` and the activation is rejected
  - a blank id is `malformed_contract`
  - the same `instance_id` is SameInstance
  - a new one is NewInstance
- **Subscriptions and snapshots are renewed on every reconnect, whatever the
  continuity.** A subscription belongs to a connection: dropping the selected
  owner closes its receivers (M1e), and the page attaches again on the new
  connection with a fresh `subscribe` and snapshot. SameInstance only says that
  the sessions are the same run. It never replays input or reuses a stream.
- **M2 requirement.** M2 must require `IdentitySource::Host` for any non-local
  endpoint, because a bootstrap file on this Mac says nothing about a remote
  Host.

### Endpoint-local lifecycle guards, completed

- `cli_path::sync(&ProcessControl)` runs at launch only when the selected
  endpoint grants one.
- `skiff_agent::ensure_watch(app, &ProcessControl, …)` starts or replaces
  `skiff watch` only with a token. `start_skiff_watch` and `open_skiff_agent`
  skip it without one. The watcher's socket is now the selected endpoint's,
  not a recomputed path.
- Together with M1e (spawn, stop/replace, restart, reload), every lifecycle
  action that M1 reaches now requires a token that only a local `Endpoint`
  yields.
- Left unguarded, deliberately: the explicit `install_cli` palette command (a
  user action on this Mac), the Skipper skill copy (`skiff_agent::sync`) and the
  rest of the Skipper folder, which move with the M6 Skipper APIs.

### Tests

- `skiff-core`: `pong` with and without `identity` (old JSON decodes, new
  serializes exactly); concurrent first publication, `0600`, public and corrupt
  files refused and kept.
- `skiff-client/tests/host_identity.rs`, fake peer:
  - adopted identity with the peer's own os/account, no extra frame after the
    recorded Ping, no bootstrap file, nothing from the pid
  - SameInstance, then NewInstance, then IdentityChanged, then blank refused,
    then the original Host accepted again
  - an older daemon gets the bootstrap identity and NewInstance; a reload onto a
    reporting daemon with the same store carries on
  - SameInstance still closes the old receiver and sends a new `subscribe` with
    a fresh snapshot
- `skiffd/tests/identity.rs`, real private daemon:
  - the id comes from its private store, `0600`
  - stable within a run; the adapter adopts it, and a reconnect is SameInstance
  - a reload keeps the pid and Host and gives a new instance
  - a restart keeps the Host and gives a new instance
  - a replaced store is refused on reconnect
- Desktop `held_input_survives_a_reload` (real skiffd): the connect Hello is
  `Host`; after the reload the `host_id` is the same, the instance is new and
  continuity is NewInstance. The watcher fixture runs with the local token.
- No live daemon, config or user socket is used.

### Not in M1f

- SSH transport, host picker, a `skiff-host` service, projects/files/ledger/Git
  movement, migrations, capability-driven UI, and lifecycle/mutation routing.
- The local adapter remains a Host contract in front of skiffd's legacy wire. It
  is not a Host envelope on the socket.

## M1g: session lifecycle and groups, and capability-gated surfaces

### What moved

These Tauri commands now reach skiffd only through the selected
`HostConnection`, behind two new capabilities:

- `session_lifecycle@1`: `create_session`, `kill_session`, `rename_session`,
  `set_session_theme`, `session_seen`. The Skipper folder change also ends
  Skipper's session through it.
- `session_groups@1`: `list_groups`, `save_group`, `delete_group`.

`remove_worktree`'s live-session check and the watcher's watch-pid read now use
the M1c inventory, not a raw `list_sessions`.

The Tauri command names, arguments, result JSON, legacy error text,
confirmations and UI flows are unchanged. The page still asks before End,
Kill all, Remove and Close, exactly as before.

### Contract (additive, API major 1)

- Requests:
  - `session_create { spec }` answers `session { session }`
  - `session_kill { session }` answers `ok`
  - `session_rename { session, name }` answers `session`
  - `session_set_theme { session, theme|null }` answers `session`
  - `session_seen { session }` answers `ok`
  - `group_list` answers `groups { groups }`
  - `group_save { group }` answers `group { group }`
  - `group_delete { group }` answers `ok`
- A reply of any other kind is `malformed_contract`.
- `SessionSpec` (`skiff-host-api::lifecycle`) mirrors the desktop spec:
  - It keeps the same defaults: shell role, 120×40, no peek, null name/cwd/command.
  - Paths are opaque UTF-8 host strings.
  - It has no coordinator flag. That flag is never on the legacy wire, and a core
    spec that sets it is refused before sending.
- `Group`, `Layout` and `SplitDir` (`skiff-host-api::groups`) keep the existing
  JSON. Because of the `f32` split ratio, `Request`, `Envelope`, `Response` and
  `Message` are now `PartialEq` without `Eq`.
- Session rows reuse the lossless M1c `InventorySession`.
- Mapping is exhaustive both ways (`skiff-client::host::lifecycle`), as in M1c.
- Golden fixture: `tests/fixtures/m1g.json`, including the legacy frame map.

### Local adapter

- `LegacySessions` on the same selected `Arc<Client>` sends exactly the old
  frames: `create_session`, `kill`, `rename_session`, `set_session_theme`,
  `seen`, `list_groups`, `save_group`, `delete_group`.
- **Errors keep their provenance:**
  - a daemon error is `operation_failed`
  - a decode failure is `malformed_contract`
  - a stall is `timed_out`
  - a lost connection is `transport_lost`
- The `legacy_*` helpers in `host::local` give each Tauri command its old
  result and error text.
- **Lane:** both families use the bounded read lane within the 20-second budget,
  and the status reservation stays. A project close that ends many sessions at
  once waits for slots rather than failing with `host_busy`.
- **No retry:** nothing is retried. A timed-out create or kill may have run.
- **No fallback:** a missing or wrong-major capability is refused before any
  frame, and an adapter cannot advertise a family it has no source for.

### Capabilities in the GUI: one mechanism

- `host_capabilities` returns the selected Host's `name@major` list.
- **`app/hostCaps.ts`** is the page's only path for session and terminal
  commands. It covers inventory, events, screen, attach/output/ack/input/resize/
  detach, lifecycle and groups.
  - `hostInvoke(cap, cmd, args)` refuses a command whose capability the Host
    does not advertise ("This Host does not offer …"), before any IPC.
  - Once the capabilities are known it calls `invoke` synchronously, so
    keystrokes keep their order.
  - The first call waits for the first answer.
- **`hostCan(cap)` decides whether a surface offers its action.** Unavailable
  actions are greyed in the menu bar and left out of the palette. This covers
  New session, Rename, Session theme, End session, Add pane, Remove from group,
  Mission control and the context menus (session, pane, worktree, project,
  group, selection, Kill all, Ungroup, Group theme).
- **Load points:** capabilities load at launch before the event stream, and
  again after a daemon reload.
- **Before the first answer, surfaces stay as they are.** The call itself still
  waits and checks.
- **Rust checks again on every request,** so a page bug cannot bypass a
  capability.
- **Local behavior:** the local Host advertises every family, so nothing visible
  changes.

### Audit while integrating

- **Lifecycle tokens.** `reload::plan` ran the bundled `skiffd --reload-info`
  from the daemon check without the endpoint-local token. It now takes
  `ProcessControl`, and the check plans no reload without one.
- Every desktop action that spawns, signals, reloads, links or replaces a Host
  process now requires the token:
  - daemon spawn
  - compatibility stop/replace
  - restart
  - reload plan and run
  - CLI link
  - watcher start/replace
- `install_cli` (an explicit user action on this Mac) and the memory diagnostics
  `ps` read of this Mac's processes are not Host lifecycle actions.
- **Identity and reconnect** (M1f) were re-checked on the paths M1g touches. No
  defect was found:
  - a refused identity leaves the previous Hello in place, so a changed Host
    stays refused
  - restart signals only a local peer pid under the token
  - SameInstance still renews subscriptions, because receivers close with their
    owner

### Still local (not Host APIs yet)

These still reach local code or the legacy Client directly. They stay in the
boundary baseline, assigned to later milestones. None of them is a Host API.

| Surface | Commands / code | Milestone |
|---|---|---|
| Projects, fleets, worktrees, project presentation | `list_projects`, `get_project`, `add_project`, `remove_project`, `reorder_projects`, `set_project_*`, `add_worktree`, `remove_worktree`, `init_repository`, `config.rs` | M4 |
| Git status and diffs | `git_changes`, `hidden_changes_projects`, `open_diff` | M4 |
| Files and previews | `list_dir`, `preview_file`, `read_image`, `open_file`, `open_path`, `reveal_file`, `inspect_folder`, `files_projects` | M5 |
| Ledger | `ledger_rows`, `ledger::watch` | M6 |
| Skipper and its folder, task types, watcher | `get_skiff_agent`, `set_skiff_agent`, `open_skiff_agent` (coordinator), `start_skiff_watch`, `skiff_watch_log`, `preview_skiff_folder`, `change_skiff_folder` (except its kill) | M6 |
| Agent list and commands | `list_agents`, `list_agent_sessions`, `set_agents`, `set_agent_command` (skiffd-served, legacy Client) | not assigned in #155; propose M3 with remote session creation |
| Endpoint-local lifecycle and diagnostics | `daemon_status` compatibility checks, `restart_daemon`, `reload_daemon`, `trace_set` | local only, token-guarded |

Client-owned surfaces (appearance, keys, menus, theme files, clipboard,
workspace files, traces and window state) stay in the client.

### Tests

- **Contract:** `m1g.json` round trips; exact reply kinds per request; defaults;
  an ignored coordinator field; bad roles, dimensions, layouts and directions
  refused.
- **Negotiated transport:** both families typed and separately gated; a service
  error stays typed; a wrong reply kind is malformed; nothing is sent without
  the capability.
- **Selected private socket** (`session_lifecycle.rs`): every operation compared
  with the legacy Client on one Client, covering exact frames, results, daemon
  errors, wrong-kind and unreadable replies, the lost-connection text, the
  unused second endpoint and a refused coordinator spec.
- **Fakes:** no capability, no source or another major means no call; 80
  concurrent kills complete without `host_busy` (at most 63 in flight);
  disconnect wakes a waiting call without replay.
- **Desktop entry point:** create, kill error, list and save group on the same
  connection as status, screen, inventory, updates and the terminal, plus the
  `host_capabilities` list.
- **TS** (`tests/hostCaps.test.ts`): offered call passes exact arguments; a
  missing or other-major capability is refused with no IPC; keystroke order
  holds across the first answer and later calls are synchronous; a reload
  re-reads the capabilities.

## M0 demo and handoff

M0 has zero visible behavior changes: desktop source, daemon source, protocol,
config/ledger paths and migrations are untouched. The only production connection
edit delegates the existing UnixStream split to the trait implementation that
performs the identical call. Test-only paired streams/private sockets prove the
old Ping JSON, protocol 27, ordered raw input and snapshot/output bytes still work.
Existing config/ledger fixture tests cover their unchanged on-disk contracts.

After M1g, every session-plane surface the desktop uses goes through the
selected Host, behind its own capability, and the page gates those surfaces on
the advertised capabilities. The agent list and Skipper's coordinator are the
exceptions (see M1g, Still local). Projects, Git, files, ledger and Skipper stay
local until M4–M6. The topology is decided structurally (M1f); M2 measures RTT
over a real forward.
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
