import { test } from "node:test";
import assert from "node:assert/strict";
import { moduleFromSource } from "./workspaceHarness.ts";

/** The real `loadProjects`: which calls it makes for each Host and what it
 *  shows. The Tauri side is a fake that answers as the selected Host's data
 *  plane would, or fails as it does when no skiff-host answers. */
function page(o: { remote: boolean; caps: string[]; fail?: string }) {
  const S: any = { agents: [{ id: "a" }], projects: [], closedProjects: [], projectsError: null, selectedProject: null };
  const calls: string[] = [];
  const projects = [
    { name: "web", path: "/srv/web", closed: false, worktrees: [{ path: "/srv/web", branch: "main" }] },
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
      "./hostCaps": { hostCan: (c: string) => o.caps.includes(c), hostInvoke: async () => [] },
      "../diagnostics/stalls": { work() {} },
      "../workspace/files": { loadFilesSetting: async () => void calls.push("files_projects") },
      "../workspace/changes": { loadChangesSetting: async () => void calls.push("hidden_changes_projects") },
      "../appearance/themes": { loadProjectThemes: async () => void calls.push("project_themes") },
      "./skiffAgent": { loadSkiffAgent: async () => void calls.push("get_skiff_agent") },
      "./stateQueries": { place: () => null },
    },
    { window: { addEventListener() {} }, document: { hasFocus: () => true }, Promise, Map, Set, console, JSON, performance },
    "",
  );
  return { mod, S, calls };
}

test("a remote Host with a data plane shows its own projects and reads no setting of this Mac", async () => {
  const p = page({ remote: true, caps: ["projects"] });
  await p.mod.loadProjects();
  assert.deepEqual(p.calls, ["list_projects"]);
  assert.deepEqual(p.S.projects.map((x: any) => x.path), ["/srv/web"]);
  assert.deepEqual(p.S.closedProjects.map((x: any) => x.name), ["old"]);
  assert.equal(p.S.projectsError, null);
});

test("a remote Host without a data plane shows its sessions only: no project read at all", async () => {
  const p = page({ remote: true, caps: [] });
  p.S.projects = [{ name: "stale", worktrees: [] }];
  await p.mod.loadProjects();
  assert.deepEqual(p.calls, []);
  assert.equal(p.S.projects.length, 0);
  assert.equal(p.S.projectsError, null);
});

test("this Mac reads its projects through the data plane, with its own settings", async () => {
  const p = page({ remote: false, caps: ["projects"] });
  await p.mod.loadProjects();
  assert.ok(p.calls.includes("list_projects"));
  for (const c of ["files_projects", "hidden_changes_projects", "project_themes", "get_skiff_agent"]) assert.ok(p.calls.includes(c), c);
});

test("this Mac without its service still asks, and the cause shows; nothing else stands in", async () => {
  const p = page({ remote: false, caps: [], fail: "skiff-host did not start on this Mac. Projects and Git need it." });
  await p.mod.loadProjects();
  assert.ok(p.calls.includes("list_projects"));
  assert.equal(p.S.projects.length, 0);
  assert.match(p.S.projectsError, /skiff-host did not start/);
});
