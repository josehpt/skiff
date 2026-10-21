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

test("a lost link turns input off once and starts one reconnect", async () => {
  const h = hosts({ selected: "devbox", hosts: [devbox] });
  await h.mod.loadHosts();
  let reconnects = 0;
  h.mod.watchHostLink(async () => {
    reconnects++;
  });
  await flush();
  const lost = h.listeners.get("skiff:host-lost")!;
  lost();
  lost();
  assert.equal(h.mod.hostConnected(), false);
  assert.equal(reconnects, 1);
  h.mod.setHostConnected(true);
  lost();
  assert.equal(reconnects, 2);
});
