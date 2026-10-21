import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource, workspaceHarness } from "./workspaceHarness.ts";

const values = (set: Set<string>) => [...set];

test("Edit → Apply preserves overlapping explicit project and fleet inclusions", async () => {
  const h = workspaceHarness();
  h.fixture("/work.skiffw", ["/a"], ["F"]);
  await h.profile.openWorkspace("/work.skiffw");
  h.profile.editWorkspace();
  h.button("Apply").click();
  assert.equal(h.profile.workspaceEdited(), false);
  h.profile.saveWorkspace();
  await flush();
  assert.deepEqual(h.files.get("/work.skiffw").profile.projects, ["/a"]);
  h.S.projects[0].fleet = null;
  assert.equal(h.profile.shows(h.S.projects[0]), true);
});

test("removing fleet coverage preserves explicit projects but not fleet-only projects", async () => {
  const h = workspaceHarness();
  h.S.projects[1].fleet = "F";
  h.fixture("/work.skiffw", ["/a"], ["F"]);
  await h.profile.openWorkspace("/work.skiffw");
  h.profile.editWorkspace();
  const fleet = h.elements().find((el) => el.tag === "label" && el.children.some((c) => c.textContent === "F"))!.children[0];
  fleet.checked = false;
  fleet.fire("change");
  h.button("Apply").click();
  assert.equal(h.profile.shows(h.S.projects[0]), true);
  assert.equal(h.profile.shows(h.S.projects[1]), false);
});

test("opening and creating a workspace reset All work; a rejected open preserves it", async () => {
  const h = workspaceHarness();
  h.fixture("/a.skiffw");
  h.fixture("/b.skiffw", ["/b"]);
  await h.profile.openWorkspace("/a.skiffw");
  h.profile.setLedgerAllWork(true);
  await h.profile.openWorkspace("/missing.skiffw");
  assert.equal(h.profile.ledgerScope(), null);
  await h.profile.openWorkspace("/b.skiffw");
  assert.equal(h.profile.ledgerAllWork(), false);
  assert.deepEqual(values(h.profile.ledgerScope().names), ["B"]);
  h.profile.setLedgerAllWork(true);
  h.profile.newWorkspace();
  await flush();
  assert.equal(h.profile.ledgerAllWork(), false);
  assert.deepEqual(values(h.profile.ledgerScope().names), []);
});

test("All work toggles preserve edits and never show the leave prompt", async () => {
  const h = workspaceHarness();
  h.fixture("/work.skiffw");
  await h.profile.openWorkspace("/work.skiffw");
  h.profile.editWorkspace();
  h.elements().find((el) => el.type === "text")!.value = "Edited";
  h.button("Apply").click();
  h.profile.setLedgerAllWork(true);
  h.profile.setLedgerAllWork(false);
  assert.equal(h.profile.workspaceEdited(), true);
  assert.equal(h.elements().some((el) => el.textContent === "Don't Save"), false);
});

for (const route of ["skiff:quit-requested", "window-close"]) {
  for (const decision of ["Cancel", "Don't Save", "Save", "failed Save", "cancelled Save As"]) {
    test(`${route}: ${decision} uses the workspace leave guard`, async () => {
      const h = workspaceHarness();
      await h.profile.startWorkspaceQuitGuard();
      await h.profile.startWorkspaces();
      assert.equal(h.calls.guarded, true);
      if (decision === "cancelled Save As") {
        h.profile.newWorkspace();
        await flush();
      } else {
        h.fixture("/work.skiffw");
        await h.profile.openWorkspace("/work.skiffw");
        h.profile.editWorkspace();
      }
      h.elements().find((el) => el.type === "text")!.value = "Edited";
      h.button("Apply").click();
      h.calls.failSave = decision === "failed Save";
      h.events.get(route)!();
      // A duplicate native/window request must not open another prompt or exit.
      h.events.get(route)!();
      assert.equal(h.elements().filter((el) => el.textContent === "Don't Save").length, 1);
      h.button(decision.includes("Save") && decision !== "Don't Save" ? "Save" : decision).click();
      await flush();
      const exits = decision === "Save" || decision === "Don't Save";
      assert.equal(h.calls.approvedExits, exits ? 1 : 0);
      assert.equal(h.profile.workspaceEdited(), decision !== "Save");
      assert.equal(h.calls.saves, decision === "Save" || decision === "failed Save" ? 1 : 0);
    });
  }
}

test("a clean native quit needs no prompt", async () => {
  const h = workspaceHarness();
  await h.profile.startWorkspaceQuitGuard();
  await h.profile.startWorkspaces();
  h.events.get("skiff:quit-requested")!();
  await flush();
  assert.equal(h.calls.approvedExits, 1);
  assert.equal(h.elements().some((el) => el.textContent === "Don't Save"), false);
});

test("mixed-scope plans keep qualifying headings without excluded children", () => {
  const ledger = moduleFromSource("../src/workspace/ledger.ts", {
    "./profile": { ledgerScope: () => ({ names: new Set(["A"]) }) },
    "../app/state": { S: {}, sessions: new Map() },
  }, {
    document: { addEventListener() {} }, window: { addEventListener() {} }, localStorage: { getItem: () => null },
  }, "\nexport const review = { setRows: (r: Entry[]) => { rows = r; }, units };\n");
  const plan = (uid: string, project: string) => ({ id: 1, uid, kind: "plan", project, state: "pending", closed_at: null });
  const task = (uid: string, parent: string, project: string) => ({ id: 2, uid, kind: "task", project, parent, state: "done", closed_at: null });
  const rows = [
    plan("global", ""), task("hidden-global-child", "global", "B"),
    plan("included", "A"), task("hidden-included-child", "included", "B"),
    plan("context", "B"), task("shown", "context", "A"), task("hidden", "context", "B"),
    plan("excluded", "B"), task("excluded-child", "excluded", "B"),
  ];
  const before = structuredClone(rows);
  ledger.review.setRows(rows);
  const units = ledger.review.units();
  assert.deepEqual(Array.from(units, (u: any) => u.r.uid), ["global", "included", "context"]);
  assert.equal(units[0].kids.length, 0);
  assert.equal(units[1].kids.length, 0);
  assert.deepEqual(Array.from(units[2].kids, (k: any) => k.r.uid), ["shown"]);
  assert.deepEqual(rows, before, "filtering must not mutate canonical rows");
});
