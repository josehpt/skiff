import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** M6: the real Ledger panel and Skipper row logic for one Host. Rows and
 *  results come from the Host's `ledger` capability, settings from its
 *  `skipper` capability; nothing names or opens a path here, and a remote
 *  Host's watcher and Skipper are never started from this app. */

function ledger(caps: string[]) {
  const calls: string[] = [];
  const shown: string[] = [];
  const mod = moduleFromSource(
    "../src/workspace/ledger.ts",
    {
      "@tauri-apps/api/core": { invoke: (cmd: string) => calls.push(`invoke ${cmd}`) },
      "@tauri-apps/api/event": { listen: () => Promise.resolve(() => {}) },
      "../app/hostCaps": {
        hostInvoke: (cap: string, cmd: string, args: unknown) => {
          if (!caps.includes(cap)) return Promise.reject(new Error(`This Host does not offer ${cap}.`));
          calls.push(`${cap} ${cmd} ${JSON.stringify(args ?? null)}`);
          return Promise.resolve([ROW]);
        },
      },
      "../terminal/peek": {
        openLedgerResult: (uid: string, title: string) => shown.push(`${uid} ${title}`),
        openPath: (p: string) => shown.push(`path ${p}`),
        previewToken: () => 1,
      },
      "../app/renderRequest": { render() {} },
      "../app/state": { S: {}, sessions: new Map() },
      "./model": { taskTitle: () => "" },
    },
    { Promise, JSON, Map, Set, document: { getElementById: () => null, addEventListener() {} } },
    "\nexports.rowsNow = () => rows; exports.errorNow = () => error; exports.taskActs = taskActs; exports.result = result;",
  );
  return { mod, calls, shown };
}

const ROW = {
  id: 1, uid: "a1b2c3d4", kind: "task", parent: null, pane_id: null, pane_gen: null, project: "", task: "Write the notes. Then stop.",
  sent_at: "x", title: null, state: "done", updated_at: "x", closed_at: null, result_exists: true,
};

test("the panel reads its rows from the selected Host's ledger, never this Mac's file", async () => {
  const t = ledger(["ledger"]);
  await t.mod.loadLedger();
  assert.deepEqual(t.calls, ["ledger ledger_rows null"]);
  assert.equal(t.mod.rowsNow().length, 1);
  assert.equal(t.mod.errorNow(), null);
});

test("a Host without the ledger shows why, with no rows and no local fallback", async () => {
  const t = ledger([]);
  await t.mod.loadLedger();
  assert.deepEqual(t.calls, []);
  assert.equal(t.mod.rowsNow().length, 0);
  assert.match(String(t.mod.errorNow()), /does not offer ledger/);
});

test("a result opens in the page's viewer by its row's uid; no path reaches an opener", () => {
  const t = ledger(["ledger"]);
  t.mod.result(ROW)();
  assert.deepEqual(t.shown, ["a1b2c3d4 Write the notes"]);
  // A result the Host cannot show gives no action at all.
  assert.equal(t.mod.result({ ...ROW, result_exists: false }), null);
  // A done task's click is its result.
  const acts = t.mod.taskActs({ r: ROW, st: "done", pane: null, where: "" }, false);
  acts.click();
  assert.equal(t.shown.length, 2);
  assert.ok(!t.shown.some((s) => s.startsWith("path ")));
});

function skipper(remote: boolean) {
  const calls: string[] = [];
  const errors: string[] = [];
  const S: any = { skiffAgent: { enabled: true, folder: "/home/me/skiff-agent", name: null, continue_on_restore: true }, agents: [] };
  const coordinator = { id: "s1", coordinator: true, state: "idle", resume: true, input_gen: 0 };
  const sessions = new Map<string, any>([["s1", coordinator]]);
  const mod = moduleFromSource(
    "../src/app/skiffAgent.ts",
    {
      "@tauri-apps/api/core": { invoke: (cmd: string) => (calls.push(`invoke ${cmd}`), Promise.resolve(null)) },
      "./hostCaps": {
        hostInvoke: (cap: string, cmd: string) => (calls.push(`${cap} ${cmd}`), Promise.resolve({ ...S.skiffAgent, tasks: [] })),
      },
      "./hosts": { isRemote: () => remote, hostName: () => "devbox" },
      "./state": { S, sessions, upsert() {} },
      "./keys": { setSkiffAgentName() {} },
      "./renderRequest": { render() {} },
      "../ui/alerts": { showError: (e: unknown) => errors.push(String(e)) },
      "../appearance/agentIcon": { agentKind: () => "shell" },
      "../workspace/selection": { keepRow() {} },
      "../workspace/view": { revealSession() {} },
      "../terminal/peek": { openPath: (p: string) => calls.push(`open ${p}`) },
    },
    { Promise, JSON, Map, Set, Date, window: { setTimeout: () => 0 }, clearTimeout() {}, requestAnimationFrame: (f: () => void) => f() },
    "",
  );
  return { mod, calls, errors, S, sessions, coordinator };
}

test("Skipper's settings come from the Host on This Mac and on a remote Host", async () => {
  for (const remote of [false, true]) {
    const t = skipper(remote);
    await t.mod.loadSkiffAgent();
    assert.deepEqual(t.calls, ["skipper get_skiff_agent"]);
  }
});

test("a remote Host's watcher, Skipper launch and restore are never started from here", async () => {
  const t = skipper(true);
  t.mod.syncSkiffAgent();
  t.mod.startSkiffWatch();
  t.mod.openSkiffAgent();
  await flush();
  assert.deepEqual(t.calls.filter((c) => c.startsWith("invoke")), [], "no start_skiff_watch or open_skiff_agent");
  // No session there: the row says where Skipper starts, and starts nothing.
  t.sessions.clear();
  t.mod.openSkiffAgent();
  assert.match(t.errors.at(-1) ?? "", /does not run on devbox.*skiff skipper/);
  // The status says who starts the watcher, never that this app tries.
  const text = t.mod.watchText("missing", t.coordinator, "Skipper", null);
  assert.match(text, /on devbox/);
  assert.doesNotMatch(text, /tries to start/);
});

test("This Mac keeps starting its own watcher and restoring Skipper", async () => {
  const t = skipper(false);
  t.mod.syncSkiffAgent();
  await flush();
  assert.ok(t.calls.includes("invoke start_skiff_watch"));
  assert.ok(t.calls.includes("invoke open_skiff_agent"));
  assert.match(t.mod.watchText("missing", t.coordinator, "Skipper", null), /tries to start/);
});
