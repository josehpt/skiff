import { open, reveal, copy } from "../platform/fileActions";
import { setRowActs } from "./rowActs";
import { sectionHeader } from "../ui/sectionHeader";
/**
 * The Files section under each worktree, for projects with `files = true`.
 * Selection previews files with their Open with setting. A folder is read
 * again when it opens, or on Refresh.
 */
import { invoke } from "@tauri-apps/api/core";
import type { Project, Worktree } from "../platform/types";
import { h } from "../ui/dom";
import { keyLabel } from "../app/keys";
import { chevron, icon } from "../ui/icons";
import { animateFold } from "../ui/fold";
import { showError } from "../ui/alerts";
import { ctxMenu } from "../ui/contextMenu";
import { render } from "../app/render";
import { storedSet } from "../app/stored";

type Entry = { name: string; dir: boolean };
type Listing = Entry[] | { error: string };

/** Projects that show the Files section, from projects.toml. */
const on = new Set<string>();
/** The last listing of each folder read. The sidebar renders from here, never waiting. */
const listings = new Map<string, Listing>();
/** Worktrees whose Files section is open. Closed by default. */
const shown = storedSet("skiff.filesOpen");
/** Open folders, by path. */
const expanded = storedSet("skiff.foldersOpen");
/** Folders being read. */
const reading = new Set<string>();

/** Counts the reads that brought a listing different from the last one, and the folders opened or closed. */
let version = 0;

const join = (dir: string, name: string) => `${dir}/${name}`;

export const showsFiles = (p: Project) => on.has(p.name);

/** What the Files section draws for `w`, as a string. Equal strings mean an equal section. */
export const filesSig = (w: Worktree) => (shown.has(w.path) ? `open:${version}` : "closed");

/** Keeps the listing when the read brought the same one, so the sidebar does not rebuild. */
function store(dir: string, l: Listing) {
  const known = listings.get(dir);
  if (known && JSON.stringify(known) === JSON.stringify(l)) return;
  listings.set(dir, l);
  version++;
}

/** Reads which projects show files. Called with every project reload. */
export async function loadFilesSetting() {
  const names = await invoke<string[]>("files_projects").catch(() => []);
  on.clear();
  for (const n of names) on.add(n);
}

export async function setShowFiles(p: Project, show: boolean) {
  try {
    await invoke("set_project_files", { project: p.name, on: show });
    if (show) on.add(p.name);
    else on.delete(p.name);
    render();
  } catch (e) {
    showError(e);
  }
}

function load(dir: string) {
  if (reading.has(dir)) return;
  reading.add(dir);
  invoke<Entry[]>("list_dir", { path: dir })
    .then((entries) => store(dir, entries))
    .catch((e) => {
      const msg = String(e);
      // Deleted since its parent was read: forget it and read the parent
      // again, so its row goes. No note. (os error 2 and 3: not found.)
      const parent = dir.slice(0, dir.lastIndexOf("/"));
      if (/\(os error [23]\)/.test(msg) && listings.has(parent)) {
        for (const d of [...listings.keys()]) if (d === dir || d.startsWith(dir + "/")) listings.delete(d);
        for (const d of [...expanded]) if (d === dir || d.startsWith(dir + "/")) expanded.delete(d);
        version++;
        load(parent);
      } else store(dir, { error: msg });
    })
    .finally(() => {
      reading.delete(dir);
      render();
    });
}

/** Reads again `root` and every open folder under it. */
function refresh(root: string) {
  load(root);
  for (const d of expanded) if (d.startsWith(root + "/")) load(d);
}

/** Opening reads again, so the tree is as fresh as the last click. */
function toggle(dir: string, set: Set<string>) {
  animateFold(`${set === shown ? "files" : "file"}:${dir}`, !set.has(dir), () => {
    if (set.has(dir)) set.delete(dir);
    else {
      set.add(dir);
      refresh(dir);
    }
    version++;
    render();
  });
}

export function filesBlock(w: Worktree): HTMLElement {
  const isOpen = shown.has(w.path);
  const block = h("div", "files-block");
  const glyph = icon('<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"></path><path d="M14 3v5h5"></path>');
  const key = `files:${w.path}`;
  // A click selects the row on mousedown (keyboard.ts), then opens or closes it.
  const head = sectionHeader(key, "Files", isOpen, glyph, () => toggle(w.path, shown));
  setRowActs(key, { preview: { kind: "folder", path: w.path }, fold: folds(w.path, shown) });
  head.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    e.stopPropagation();
    ctxMenu.open(e.clientX, e.clientY, "Files", [
      { icon: "schedule-refresh-cw", label: "Refresh", run: () => refresh(w.path) },
      { icon: "indicators-square-arrow-out-up-right", label: "Show in file manager", run: () => open(w.path) },
    ]);
  });
  block.appendChild(head);
  if (isOpen) {
    const rows = h("div", "files-rows");
    rows.dataset.kids = key;
    folder(rows, w.path, 0);
    block.appendChild(rows);
  }
  return block;
}

function folder(into: HTMLElement, dir: string, depth: number) {
  const l = listings.get(dir);
  if (!l) {
    // Open from a past run: read it the first time it shows.
    load(dir);
    into.appendChild(note("…", depth));
  }
  else if (!Array.isArray(l)) into.appendChild(note(l.error, depth, true));
  else if (!l.length) into.appendChild(note("Empty", depth));
  else for (const e of l) {
    const path = join(dir, e.name);
    into.appendChild(row(e, path, depth));
    if (e.dir && expanded.has(path)) {
      // The open folder's rows sit in one box, so they can grow and fold together.
      const sub = h("div", "files-rows");
      sub.dataset.kids = `file:${path}`;
      folder(sub, path, depth + 1);
      into.appendChild(sub);
    }
  }
}

function row(e: Entry, path: string, depth: number): HTMLElement {
  // A folder read as empty has nothing to open: no chevron, no fold.
  const l = e.dir ? listings.get(path) : undefined;
  const foldable = e.dir && !(Array.isArray(l) && !l.length);
  const cls = ["file-row"];
  if (e.dir) cls.push("dir");
  if (e.name.startsWith(".")) cls.push("hidden-file");
  const r = h("button", cls.join(" "));
  r.type = "button";
  r.dataset.key = `file:${path}`;
  if (foldable) r.dataset.fold = r.dataset.key;
  setRowActs(r.dataset.key, e.dir
    ? { preview: { kind: "folder", path }, fold: foldable ? folds(path, expanded) : undefined }
    : { preview: { kind: "file", path }, open: () => open(path) });
  r.style.setProperty("--depth", String(depth));
  if (foldable) r.setAttribute("aria-expanded", String(expanded.has(path)));
  // The mousedown selected the row and its preview (keyboard.ts). A click on a
  // folder opens or closes it; a double-click on a file opens it.
  r.addEventListener("click", (m) => {
    // detail counts clicks across the re-render the first click causes.
    if (foldable) toggle(path, expanded);
    else if (m.detail >= 2) open(path);
  });
  const lead = h("span", "file-lead");
  if (foldable) lead.appendChild(chevron(expanded.has(path)));
  r.append(lead, h("span", "file-name", e.name));
  r.addEventListener("contextmenu", (m) => {
    m.preventDefault();
    m.stopPropagation();
    ctxMenu.open(m.clientX, m.clientY, e.name, e.dir
      ? [
          { icon: "schedule-refresh-cw", label: "Refresh", run: () => refresh(path) },
          { icon: "indicators-square-arrow-out-up-right", label: "Show in file manager", hint: keyLabel("project-folder"), run: () => open(path) },
          { icon: "code-copy", label: "Copy path", hint: keyLabel("project-copy-path"), run: () => copy(path) },
        ]
      : [
          { icon: "indicators-square-arrow-out-up-right", label: "Open", hint: "Enter", run: () => open(path) },
          { icon: "documents-folder-open", label: "Show in file manager", hint: keyLabel("project-folder"), run: () => reveal(path) },
          { icon: "code-copy", label: "Copy path", hint: keyLabel("project-copy-path"), run: () => copy(path) },
        ]);
  });
  return r;
}

/** Space, Left and Right on a folder row: `open` sets it, no argument flips it. */
const folds = (path: string, set: Set<string>) => (open?: boolean) => {
  if (open === undefined || open !== set.has(path)) toggle(path, set);
};

function note(text: string, depth: number, error = false): HTMLElement {
  const n = h("div", "file-note" + (error ? " error" : ""), text);
  n.style.setProperty("--depth", String(depth));
  return n;
}
