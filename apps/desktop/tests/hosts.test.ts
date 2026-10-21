import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real hosts module over a fake `invoke`, `listen` and document. */
function hosts(answer: { selected: string | null; hosts: { destination: string; socket: string; clipboard: boolean }[] }) {
  const calls: [string, unknown][] = [];
  const listeners = new Map<string, () => void>();
  let reloads = 0;
  const invoke = async (command: string, args?: unknown) => {
    calls.push([command, args]);
    if (command === "list_hosts") return structuredClone(answer);
  };
  const document = { title: "", getElementById: () => null };
  const mod = moduleFromSource(
    "../src/app/hosts.ts",
    {
      "@tauri-apps/api/core": { invoke },
      "@tauri-apps/api/event": { listen: async (name: string, run: () => void) => void listeners.set(name, run) },
      "../ui/contextMenu": { ctxMenu: { open() {} } },
      "../ui/confirm": { confirmAction: async () => true },
      "../ui/dialogParts": { actionDialog() {} },
      "../ui/dom": { h: () => ({}) },
      "../ui/alerts": { showError(e: unknown) { throw e; } },
    },
    { document, location: { reload: () => reloads++ }, Promise, Set, structuredClone },
    "\nexports.__pick = pick;",
  );
  return { mod, calls, listeners, document, reloads: () => reloads };
}

const devbox = { destination: "devbox", socket: "/run/user/1000/skiff/skiffd.sock", clipboard: false };

test("this Mac writes the clipboard; a remote Host only after it is allowed", async () => {
  const local = hosts({ selected: null, hosts: [devbox] });
  await local.mod.loadHosts();
  assert.equal(local.mod.isRemote(), false);
  assert.equal(local.mod.clipboardWrites(), true);
  assert.equal(local.document.title, "Skiff");

  const remote = hosts({ selected: "devbox", hosts: [devbox, { ...devbox, destination: "other", clipboard: true }] });
  await remote.mod.loadHosts();
  assert.equal(remote.mod.isRemote(), true);
  assert.equal(remote.mod.hostName(), "devbox");
  assert.equal(remote.document.title, "Skiff — devbox");
  // Another Host's opt-in does not count for this one.
  assert.equal(remote.mod.clipboardWrites(), false);

  const allowed = hosts({ selected: "devbox", hosts: [{ ...devbox, clipboard: true }] });
  await allowed.mod.loadHosts();
  assert.equal(allowed.mod.clipboardWrites(), true);
});

test("picking a Host saves it, then reloads the page; the shown Host does nothing", async () => {
  const h = hosts({ selected: null, hosts: [devbox] });
  await h.mod.loadHosts();
  h.mod.__pick(null);
  await flush();
  assert.equal(h.calls.filter(([c]) => c === "select_host").length, 0);
  h.mod.__pick("devbox");
  await flush();
  assert.equal(JSON.stringify(h.calls.at(-1)), JSON.stringify(["select_host", { destination: "devbox" }]));
  assert.equal(h.reloads(), 1);
});

test("a lost link turns input off once and starts one recovery", async () => {
  const h = hosts({ selected: "devbox", hosts: [devbox] });
  await h.mod.loadHosts();
  let reconnects = 0;
  let onlines = 0;
  let finish!: () => void;
  h.mod.watchHostLink(
    () => {
      reconnects++;
      return new Promise<void>((r) => (finish = r));
    },
    () => onlines++,
  );
  await flush();
  const lost = h.listeners.get("skiff:host-lost")!;
  lost();
  assert.equal(h.mod.hostConnected(), false);
  assert.equal(reconnects, 1);
  finish();
  await flush();
  assert.equal(h.mod.hostConnected(), true);
  assert.equal(onlines, 1);
  lost();
  assert.equal(reconnects, 2);
});

/** Review P2 (race.mjs): the real hosts and daemon modules. The new link
 *  drops while recovery subscribes to events, and that subscription fails.
 *  The loss is kept, the whole recovery runs again, and only the second,
 *  clean run turns input back on and shows the Host connected. */
test("a second loss during recovery runs the whole recovery again before input is on", async () => {
  let lost!: () => void;
  const immediate = (f: () => void) => setImmediate(f);
  const hostsMod = moduleFromSource(
    "../src/app/hosts.ts",
    {
      "@tauri-apps/api/core": { invoke: async () => ({ selected: "fixture", hosts: [] }) },
      "@tauri-apps/api/event": { listen: async (_: string, cb: () => void) => void (lost = cb) },
    },
    { document: { title: "", getElementById: () => null }, Promise, setTimeout: immediate, console: { error() {} } },
    "",
  );
  await hostsMod.loadHosts();
  const noop = () => {};
  const elem = { querySelector: () => ({}), replaceChildren: noop, set textContent(_: string) {}, set title(_: string) {} };
  let muted = false;
  const mutes: boolean[] = [];
  let attaches = 0;
  let subscribes = 0;
  const daemon = moduleFromSource(
    "../src/app/daemon.ts",
    {
      "@tauri-apps/api/core": { invoke: async () => ({ connected: true }), Channel: class {} },
      "@tauri-apps/api/event": { listen: async () => {} },
      "../ui/dom": { $: () => elem },
      "./daemonRestart": { daemonBadge: noop },
      "./skiffAgent": { noteDaemon: noop },
      "./hosts": hostsMod,
      "./render": { render: noop },
      "./state": { born: new Set(), gone: new Set(), sessions: new Map(), S: {} },
      "./seen": { quietSeen: noop },
      "../terminal/terminalRuntime": {
        muteInput: (x: boolean) => {
          muted = x;
          mutes.push(x);
        },
        reattachPanes: () => attaches++,
      },
      "./hostCaps": {
        loadHostCaps: async () => {},
        hostCan: () => false,
        hostInvoke: async (_: string, cmd: string) => {
          if (cmd === "subscribe_events" && ++subscribes === 1) {
            lost();
            throw Error("second connection dropped during resubscribe");
          }
          if (cmd === "list_sessions") return [];
        },
      },
    },
    { window: { addEventListener: noop }, document: { hasFocus: () => true }, console: { error: noop }, setTimeout: immediate, Promise, Map, Set },
    "",
  );
  let reconnects = 0;
  hostsMod.watchHostLink(async () => {
    reconnects++;
    await daemon.reconnectRemote();
  }, daemon.hostOnline);
  lost();
  for (let i = 0; i < 20; i++) await flush();
  assert.equal(reconnects, 2, "the failed recovery ran again");
  assert.equal(subscribes, 2, "events were subscribed again");
  assert.equal(attaches, 1, "panes reattach only in the clean run");
  assert.equal(hostsMod.hostConnected(), true);
  assert.equal(muted, false);
  // Input stayed off until the end: never on between the two runs.
  assert.deepEqual(mutes, [true, true, false]);
});
