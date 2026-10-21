import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real remoteLaunch module. The Host's sessions and groups, its agents
 *  and the start call are fakes; menus and dialogs are recorded. */
function launch(o: { sessions?: { id: string; cwd: string }[]; groups?: { cwd?: string | null }[]; agents?: { id: string; command: string }[] } = {}) {
  const started: unknown[][] = [];
  const menus: { title: string; entries: any[] }[] = [];
  const dialogs: any[] = [];
  let refocused = 0;
  const choices = [null, ...(o.agents ?? [])];
  const mod = moduleFromSource(
    "../src/workspace/remoteLaunch.ts",
    {
      "../ui/contextMenu": { ctxMenu: { open: (_x: number, _y: number, title: string, entries: any[]) => menus.push({ title, entries }) } },
      "../ui/dialogParts": { actionDialog: (d: any) => dialogs.push(d) },
      "../ui/dom": { h: () => ({ setAttribute() {}, value: "" }) },
      "../ui/alerts": { showError(e: unknown) { throw e; } },
      "../app/daemon": { newSession: async (...args: unknown[]) => void started.push(args) },
      "../app/hosts": { hostName: () => "devbox", hostConnected: () => true },
      "../app/state": { S: { groups: o.groups ?? [] }, sessions: new Map((o.sessions ?? []).map((s) => [s.id, s])) },
      "../app/stateQueries": { launchChoices: () => choices, launchDigit: (i: number) => String(i + 1) },
      "../appearance/agentIcon": { launchIcon: () => ({}) },
      "./model": { basename: (p: string) => p.split("/").pop() },
      "./view": { refocusTerminal: () => refocused++ },
    },
    { Set, Map, Promise },
    "",
  );
  return { mod, started, menus, dialogs, refocused: () => refocused };
}

test("the folders are the ones the Host reported, once each, the asked one first", () => {
  const { mod } = launch();
  const list = [{ cwd: "/srv/b" }, { cwd: "/srv/a" }, { cwd: "/srv/b" }];
  assert.deepEqual([...mod.hostFolders(list, [{ cwd: "/srv/c" }, { cwd: null }])], ["/srv/a", "/srv/b", "/srv/c"]);
  assert.deepEqual([...mod.hostFolders(list, [], "/srv/b")], ["/srv/b", "/srv/a"]);
});

test("a typed folder is an absolute path on the Host, or empty for its home", () => {
  const { mod } = launch();
  assert.equal(mod.typedFolder("  /home/me/λ repo ", "devbox"), "/home/me/λ repo");
  assert.equal(mod.typedFolder("   ", "devbox"), null);
  // `~` and relative paths would be read on the Host as typed: refused here.
  for (const bad of ["~/repo", "repo", "./x"]) assert.throws(() => mod.typedFolder(bad, "devbox"), /absolute path on devbox/);
});

test("New session on a remote Host starts the Host's agent in a Host folder", async () => {
  const h = launch({
    sessions: [{ id: "s1", cwd: "/srv/web" }, { id: "s2", cwd: "/srv/api" }],
    groups: [{ cwd: "/srv/ops" }],
    agents: [{ id: "claude", command: "claude --host-side" }],
  });
  h.mod.openRemoteNewSession(10, 20, "/srv/web");
  const menu = h.menus[0];
  assert.equal(menu.title, "New session");
  assert.equal(menu.entries[0].head, "FOLDER ON DEVBOX");
  assert.deepEqual([...menu.entries.slice(1).map((e: any) => e.label)], ["/srv/web", "/srv/api", "/srv/ops", "Home folder", "Type a folder…"]);
  // Shell, then the Host's agents, under each folder.
  const web = menu.entries[1].sub;
  assert.deepEqual([...web.map((e: any) => [e.label, e.hint])], [["Shell", "1"], ["claude", "2"]]);
  web[1].run();
  menu.entries[4].sub[0].run();
  await flush();
  assert.deepEqual(h.started, [
    ["/srv/web", "claude --host-side", "claude", "agent"],
    [null, null, "", "shell"],
  ]);
});

test("a typed folder opens the start menu; Cancel starts nothing and gives the keys back", async () => {
  const h = launch({ agents: [{ id: "codex", command: "codex" }] });
  h.mod.openRemoteNewSession(0, 0);
  h.menus[0].entries.at(-1).run();
  const d = h.dialogs[0];
  assert.equal(d.title, "New session on devbox");
  d.content[0].value = "relative";
  assert.throws(() => d.submit(), /absolute path/);
  d.content[0].value = "/srv/λ";
  d.submit();
  d.onClose();
  assert.equal(h.menus[1].entries[0].head, "NEW IN Λ");
  h.menus[1].entries[2].run();
  await flush();
  assert.deepEqual(h.started, [["/srv/λ", "codex", "codex", "agent"]]);

  const c = launch();
  c.mod.openRemoteNewSession(0, 0);
  c.menus[0].entries.at(-1).run();
  c.dialogs[0].onClose();
  assert.equal(c.menus.length, 1);
  assert.equal(c.refocused(), 1);
  assert.deepEqual(c.started, []);
});
