import { open, reveal, copy } from "../platform/fileActions";
import { sectionHeader } from "../ui/sectionHeader";
/**
 * The Changes section under each worktree, and the +/- counts on its row.
 * It lists changes against HEAD, shows their diffs in the peek, and opens files
 * in the selected apps.
 */
import { invoke } from "@tauri-apps/api/core";
import type { Project, Worktree } from "../platform/types";
import { h } from "../ui/dom";
import { keyLabel } from "../app/keys";
import { icon } from "../ui/icons";
import { animateFold } from "../ui/fold";
import { showError } from "../ui/alerts";
import type { MenuEntry } from "../ui/menu";
import { ctxMenu } from "../ui/contextMenu";
import { branchName } from "./model";
import { setRowActs } from "./rowActs";
import { select } from "../app/keyboard";
import { render, scheduleRender } from "../app/render";
import { collapsed } from "../app/state";

import { storedSet } from "../app/stored";
import { isRemote } from "../app/hosts";

type Change = { path: string; status: "M" | "A" | "D" | "U"; added: number | null; removed: number | null };

/** Projects with `changes = false` in projects.toml. Changes show by default. */
const hidden = new Set<string>();

export const showsChanges = (p: Project) => !hidden.has(p.name);

/** Reads which projects hide changes. Called with every project reload. */
export async function loadChangesSetting() {
  const names = await invoke<string[]>("hidden_changes_projects").catch(() => []);
  hidden.clear();
  for (const n of names) hidden.add(n);
}

export async function setShowChanges(p: Project, show: boolean) {
  try {
    await invoke("set_project_changes", { project: p.name, on: show });
    if (show) hidden.delete(p.name);
    else hidden.add(p.name);
    render();
  } catch (e) {
    showError(e);
  }
}

/** The last read of each worktree. The sidebar renders from here, never waiting. */
const lists = new Map<string, Change[]>();
const loading = new Set<string>();
/** Worktrees whose Changes section is open. Closed by default. */
const shown = storedSet("skiff.changesOpen");

/** Counts the reads that brought a list different from the last one. */
let version = 0;

/** What the Changes section and the counts draw for `w`, as a string. Equal strings mean an equal section. */
export const changesSig = (w: Worktree) => `${shown.has(w.path) ? "open" : "closed"}:${version}`;

/** Keeps the list when the read brought the same one, so the sidebar does not rebuild. */
function store(wt: string, list: Change[]) {
  const known = lists.get(wt);
  if (known && JSON.stringify(known) === JSON.stringify(list)) return;
  lists.set(wt, list);
  version++;
}

function load(wt: string) {
  if (loading.has(wt)) return;
  loading.add(wt);
  invoke<Change[]>("git_changes", { path: wt })
    // Not a repository, or git is missing: the worktree shows no changes.
    .catch((): Change[] => [])
    .then((list) => store(wt, list))
    .finally(() => {
      loading.delete(wt);
      scheduleRender();
    });
}

/** Reads the known worktrees in `only` again, or every known worktree without it. */
export function reloadChanges(only?: Iterable<string>) {
  if (!only) {
    for (const wt of lists.keys()) load(wt);
    return;
  }
  for (const wt of only) if (lists.has(wt)) load(wt);
}

function listFor(w: Worktree): Change[] | undefined {
  const list = lists.get(w.path);
  if (!list) load(w.path);
  return list;
}

/** `+12 −3` for the worktree row, or null when nothing changed. */
export function changeCounts(w: Worktree): HTMLElement | null {
  const list = listFor(w);
  if (!list?.length) return null;
  let add = 0;
  let del = 0;
  for (const c of list) {
    add += c.added ?? 0;
    del += c.removed ?? 0;
  }
  const el = h("span", "wt-diff");
  el.append(h("span", "diff-add", `+${add}`), h("span", "diff-del", `−${del}`));
  return el;
}

/** Whether the worktree has changes, as last read. */
export const hasChanges = (w: Worktree) => !!lists.get(w.path)?.length;

const join = (wt: string, rel: string) => `${wt}/${rel}`;

function toggle(wt: string) {
  animateFold(`changes:${wt}`, !shown.has(wt), () => {
    if (shown.has(wt)) shown.delete(wt);
    else {
      shown.add(wt);
      load(wt);
    }
    render();
  });
}

/** Review all changes: the worktree's Changes row becomes current, and its diff takes the keys. */
export function reviewChanges(w: Worktree) {
  // The row must be in the list to be current.
  collapsed.delete(w.path);
  render();
  select(`changes:${w.path}`, true, true);
}

/** The section, or null when the worktree has no changes. */
export function changesBlock(w: Worktree): HTMLElement | null {
  const list = listFor(w);
  if (!list?.length) return null;
  const isOpen = shown.has(w.path);
  const block = h("div", "files-block changes-block");
  const glyph = icon('<path d="M12 3v12"></path><path d="M6 9h12"></path><path d="M6 21h12"></path>');
  const key = `changes:${w.path}`;
  // A click selects the row on mousedown (keyboard.ts), then opens or closes it.
  const head = sectionHeader(key, "Changes", isOpen, glyph, () => toggle(w.path));
  // A remote Host's diffs need a diff read (M4 follow-up): its list shows, its preview does not.
  const remote = isRemote();
  setRowActs(key, {
    preview: remote ? undefined : { kind: "diff", wt: w.path, branch: branchName(w), wait: true },
    fold: (open) => { if (open === undefined || open !== shown.has(w.path)) toggle(w.path); },
  });
  // Open, the rows show how many.
  if (!isOpen) head.appendChild(h("span", "changes-n", String(list.length)));
  head.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    e.stopPropagation();
    ctxMenu.open(e.clientX, e.clientY, "Changes", [
      ...(remote ? [] : [{ icon: "code-git-branch", label: "Review all changes", run: () => reviewChanges(w) }]),
      { icon: "schedule-refresh-cw", label: "Refresh", run: () => load(w.path) },
    ]);
  });
  block.appendChild(head);
  if (isOpen) {
    const rows = h("div", "files-rows");
    rows.dataset.kids = key;
    for (const c of list) rows.appendChild(row(w, c));
    block.appendChild(rows);
  }
  return block;
}

function row(w: Worktree, c: Change): HTMLElement {
  const wt = w.path;
  const path = join(wt, c.path);
  const key = `change:${path}`;
  const gone = c.status === "D";
  // The row's diff shows while it is current. Shift+Enter opens the file itself.
  // A remote Host's file is that Host's: its path is copied, never previewed or opened here.
  const remote = isRemote();
  setRowActs(key, remote ? {} : { preview: { kind: "diff", wt, branch: branchName(w), file: c.path }, open: gone ? undefined : () => open(path) });
  const show = () => select(key, true, true);
  const slash = c.path.lastIndexOf("/");
  const name = c.path.slice(slash + 1);
  const dir = slash < 0 ? "" : c.path.slice(0, slash);
  const r = h("button", "file-row change-row" + (gone ? " st-gone" : "") );
  r.type = "button";
  r.dataset.key = key;
  // The mousedown selects the row, and its diff shows (keyboard.ts). Enter
  // gives the diff the keys; Shift+Enter opens the file (keyboard.ts).
  const st = h("span", `change-st st-${c.status}`, c.status);
  const counts = h("span", "change-counts");
  if (c.added === null) counts.textContent = "bin";
  else {
    if (c.added) counts.appendChild(h("span", "diff-add", `+${c.added}`));
    if (c.removed) counts.appendChild(h("span", "diff-del", `−${c.removed}`));
  }
  r.append(st, h("span", "file-name", name), h("span", "change-dir", dir), counts);
  r.addEventListener("contextmenu", (m) => {
    m.preventDefault();
    m.stopPropagation();
    const items: MenuEntry[] = remote ? [] : [
      { icon: "code-git-branch", label: "Show diff", hint: "Enter", run: show },
    ];
    if (!gone && !remote) items.push(
      { icon: "indicators-square-arrow-out-up-right", label: "Open file", hint: "Shift+Enter", run: () => open(path) },
      { icon: "documents-folder-open", label: "Show in file manager", hint: keyLabel("project-folder"), run: () => reveal(path) },
    );
    items.push({ icon: "code-copy", label: "Copy path", hint: keyLabel("project-copy-path"), run: () => copy(path) });
    ctxMenu.open(m.clientX, m.clientY, name, items);
  });
  return r;
}
