import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

/** The real `loadProjects`: which calls it makes for each Host and what it
 *  shows. The Tauri side is a fake that answers as the selected Host's data
 *  plane would, or fails as it does when no skiff-host answers. */
function page(o: { remote: boolean; caps: string[]; fail?: string; capsFail?: string; dataPlane?: unknown }) {
  const S: any = { agents: [{ id: "a" }], projects: [], closedProjects: [], projectsError: null, selectedProject: null, dataState: "loading", dataNote: null };
  const calls: string[] = [];
  const projects = [
    { name: "web", path: "/srv/web", closed: false, files: true, theme: "lagoon", worktrees: [{ path: "/srv/web", branch: "main" }] },
    { name: "old", path: "/srv/old", closed: true, worktrees: [] },
  ];
  const mod = moduleFromSource(
    "../src/app/daemon.ts",
    {
      "@tauri-apps/api/core": {
        invoke: async (cmd: string) => {
          calls.push(cmd);
          if (cmd === "list_projects") {
            if (o.fail) throw o.fail;
            return projects;
          }
        },
        Channel: class {},
      },
      "@tauri-apps/api/event": { listen: async () => {} },
      "./hosts": { isRemote: () => o.remote, hostName: () => "devbox" },
      "./render": { render() {} },
      "./state": { S, sessions: new Map(), removeErrors: new Map(), OTHER: "\u0000other" },
      "./hostCaps": {
        hostCan: (c: string) => o.caps.includes(c),
        hostInvoke: async () => [],
        loadHostCaps: async () => {
          calls.push("host_capabilities");
          if (o.capsFail) throw o.capsFail;
        },
      },
      "./dataPlane": o.dataPlane ?? {},
      "../terminal/peek": { refreshHostDiff() {} },
      "../diagnostics/stalls": { work() {} },
      "../workspace/files": { filesFrom: (l: any[]) => void calls.push(`files ${l.filter((p) => p.files).map((p) => p.name)}`) },
      "../workspace/changes": { reloadChanges() {} },
      "../appearance/themes": { projectThemesFrom: (l: any[]) => void calls.push(`themes ${l.filter((p) => p.theme).map((p) => `${p.name}=${p.theme}`)}`) },
      "./skiffAgent": { loadSkiffAgent: async () => void calls.push("get_skiff_agent") },
      "./stateQueries": { place: () => null },
    },
    { window: { addEventListener() {} }, document: { hasFocus: () => true }, Promise, Map, Set, console, JSON, performance },
    "",
  );
  return { mod, S, calls };
}

test("a remote Host with a data plane shows its own projects, with their settings from its rows", async () => {
  const p = page({ remote: true, caps: ["projects"] });
  await p.mod.loadProjects();
  assert.deepEqual(p.calls, ["list_projects", "files web", "themes web=lagoon"]);
  assert.deepEqual(p.S.projects.map((x: any) => x.path), ["/srv/web"]);
  assert.deepEqual(p.S.closedProjects.map((x: any) => x.name), ["old"]);
  assert.equal(p.S.projectsError, null);
  assert.equal(p.S.dataState, "ready");
});

test("a remote Host without a data plane shows its sessions only: no project read at all", async () => {
  const p = page({ remote: true, caps: [] });
  p.S.projects = [{ name: "stale", worktrees: [] }];
  await p.mod.loadProjects();
  assert.deepEqual(p.calls, []);
  assert.equal(p.S.projects.length, 0);
  assert.equal(p.S.projectsError, null);
  // Not "loading" forever: the list says it is out of reach, and the data
  // plane check fills in why.
  assert.equal(p.S.dataState, "unavailable");
});

test("this Mac reads its projects and their settings through the data plane, as a remote Host does", async () => {
  const p = page({ remote: false, caps: ["projects"] });
  await p.mod.loadProjects();
  assert.ok(p.calls.includes("list_projects"));
  // Project settings come with the Host's rows; only Skipper is still this Mac's.
  for (const c of ["files web", "themes web=lagoon", "get_skiff_agent"]) assert.ok(p.calls.includes(c), c);
  for (const c of ["files_projects", "hidden_changes_projects", "project_themes"]) assert.ok(!p.calls.includes(c), c);
});

test("this Mac without its service still asks, and the cause shows; nothing else stands in", async () => {
  const p = page({ remote: false, caps: [], fail: "skiff-host did not start on this Mac. Projects and Git need it." });
  await p.mod.loadProjects();
  assert.ok(p.calls.includes("list_projects"));
  assert.equal(p.S.projects.length, 0);
  assert.match(p.S.projectsError, /skiff-host did not start/);
  assert.equal(p.S.dataState, "unavailable");
  assert.match(p.S.dataNote, /skiff-host did not start/);
});

test("review M4 P2: a failed project or capability read on a live data connection is read again by the next check", async () => {
  // The real dataPlane module over one live connection, and the real page hooks.
  const live = (o: any) => {
    const dataPlane = moduleFromSource(
      "../src/app/dataPlane.ts",
      { "@tauri-apps/api/core": { invoke: async () => ({ ready: true, waiting: false, host_id: "H", instance_id: "i", connection: 1, capabilities: [], message: null }) } },
      { window: { setTimeout() {}, clearTimeout() {} }, Promise },
      "",
    );
    let hooks: any;
    const p = page(Object.assign(o, { remote: true, caps: ["projects"], dataPlane: { watchDataPlane: (h: unknown) => (hooks = h) } }));
    p.mod.followDataPlane();
    const count = (cmd: string) => p.calls.filter((c) => c === cmd).length;
    return { p, o, count, check: () => dataPlane.checkDataPlane(hooks) };
  };
  for (const [what, cmd] of [["fail", "list_projects"], ["capsFail", "host_capabilities"]]) {
    const o: any = { [what]: `the ${cmd} read failed` };
    const l = live(o);
    // The first ready check reads, and that read fails.
    await l.check();
    assert.equal(l.count(cmd), 1, what);
    // Repaired: the next check on the same connection reads again, once.
    o[what] = undefined;
    for (let i = 0; i < 10; i++) await l.check();
    assert.equal(l.count(cmd), 2, what);
    assert.equal(l.p.S.dataState, "ready", what);
  }
});
