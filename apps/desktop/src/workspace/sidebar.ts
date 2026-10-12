import { worktreeLines, type Line } from "./sidebarLines";
import { groupRenameInput } from "./rename";
/** The session tree and its focus, caret, and hover restoration. */

import { filledOf } from "./layoutSlots";
import { agentIcon, agentKind } from "../appearance/agentIcon";
import { ink } from "../appearance/appTheme";
import { appliedTheme, ownTheme, signatureColor } from "../appearance/themes";
import { glyph } from "./layoutIcon";
import { agentName, basename, branchName, taskTitle } from "./model";
import type { Group, Project, SessionInfo, Worktree } from "../platform/types";

import { changeCounts, changesBlock, changesSig, showsChanges } from "./changes";
import { filesBlock, filesSig, showsFiles } from "./files";
import { $, button, h } from "../ui/dom";
import { branchIcon, chevron, icon, plusIcon } from "../ui/icons";

import { fleetMenu, groupMenu, projectMenu, rowMenu, worktreeMenu } from "./menus";

import { launchMenu } from "../app/panels";

import { renameRow, startRename, startSessionRename } from "./rename";

import { itemKey, listItems } from "../app/keyboard";
import { clearSelection, keepRow, selectRange, toggleSelect, toggleSelectGroup } from "./selection";
import { startPreview } from "../terminal/peek";
import { stateIcon } from "../appearance/stateIcon";
import { collapsed, DEFAULT_ACCENT, removeErrors, removing, S, selectedWorktree, sessions } from "../app/state";
import { accent, awayPlaces, currentWorktree, place } from "../app/stateQueries";

import { dragSessions, revealSession, selectProject, selectWorktree, showGroup } from "./view";
import { addProject } from "../app/panels";
import { fleetKey, folded, isOpen, openToFocused, projectBadge, projectKey, rollup, setOpen, treeSections, type Section } from "./tree";
import { render } from "../app/renderRequest";

/** 20px lines, flush against whatever follows. */
function sessionBlock(lines: Line[], color: string): HTMLElement {
  const block = h("div", "sessions-block");
  const n = lines.length;
  block.style.height = `calc(${n ? Math.max(40, n * 20) : 0} * var(--u))`;
  for (const l of lines) {
    if (l.group) block.appendChild(groupRow(l.group, l.hasPrevious, l.hasNext));
    else if (l.pointer) block.appendChild(pointerRow(l.pointer));
    else block.appendChild(sessionRow(l.session, color, l));
  }
  return block;
}

function sessionRow(s: SessionInfo, color: string, o: { branch?: boolean; in?: Group; groupRail?: boolean }): HTMLElement {
  if (S.renamingSession === s.id) return renameRow(s, color);
  // State shows only when it matters: waiting stands out, working pulses, done fades.
  const cls = ["session-row", `st-${s.state}`];
  if (o.in) cls.push("nested");
  if (o.groupRail) cls.push("group-rail");
  if (S.selection.includes(s.id)) cls.push("selected");
  const row = h("button", cls.join(" "));
  row.type = "button";
  row.dataset.session = s.id;
  if (o.in) row.dataset.inGroup = o.in.id;
  row.addEventListener("mousedown", (e) => {
    // dragSessions prevents the browser's default mousedown behavior to avoid
    // text selection, so focus the row explicitly before starting the drag.
    // Otherwise Delete stays in the terminal after a mouse selection.
    if (e.button === 0) row.focus();
    dragSessions(e, S.selection.length > 1 && S.selection.includes(s.id) ? [...S.selection] : [s.id]);
  });
  row.addEventListener("click", (e) => {
    // detail counts clicks across the re-render the first click causes.
    if (e.detail >= 2 && !e.shiftKey && !e.ctrlKey && !e.metaKey) startSessionRename(s.id);
    else if (e.shiftKey) selectRange(s.id);
    else if (e.ctrlKey || e.metaKey) toggleSelect(s.id);
    else {
      // A plain click opens and ends any selection. It never picks.
      clearSelection();
      revealSession(s.id);
      // The keys stay on the row, as on a picked group's, so Delete can end it.
      // Typing still reaches the terminal: see passToTerminal in keyboard.ts.
      requestAnimationFrame(() => keepRow(s.id));
    }
  });
  row.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    rowMenu(s, e.clientX, e.clientY);
  });
  row.style.setProperty("--pc", color);
  if (o.in) row.style.setProperty("--gc", groupColor(o.in));
  row.setAttribute("aria-label", `${agentName(s)}, ${taskTitle(s)}, ${s.state}`);
  row.append(agentIcon(s), h("span", "title", taskTitle(s)));
  if (o.branch) {
    const at = place(s);
    row.appendChild(h("span", "where mono", at ? branchName(at.worktree) : "other"));
  }
  const away = awayPlaces(s);
  if (away.length) {
    const names = away.map((a) => branchName(a.worktree)).join(", ");
    const tag = h("span", "where away mono", `→ ${names}`);
    row.appendChild(tag);
  }
  row.appendChild(stateIcon(s));
  return row;
}

/**
 * A session from another worktree that works in this one. Not a second
 * session: a click goes to the real one. The keys skip it.
 */
function pointerRow(s: SessionInfo): HTMLElement {
  const at = place(s);
  const from = at ? branchName(at.worktree) : basename(s.cwd);
  const row = h("div", "session-row pointer-row");
  row.append(agentIcon(s), h("span", "title", taskTitle(s)), h("span", "where mono", `from ${from}`));
  row.addEventListener("click", () => {
    clearSelection();
    revealSession(s.id);
    requestAnimationFrame(() => keepRow(s.id));
  });
  return row;
}

function errorRow(text: string): HTMLElement {
  const row = h("div", "error-row", text);
  row.setAttribute("role", "alert");
  return row;
}

let lastProjects: Project[] | null = null;
let lastGroups: Group[] | null = null;
let listGen = 0;

/**
 * Everything the tree draws, as one string. Renders run while agents work. A
 * rebuilt tree loses its hover, restarts its icons and forces a layout to put
 * the focus back, so the tree is rebuilt only when what it shows changes.
 */
function sidebarSig(): string {
  // Rows close over project, worktree and group objects. A new list brings new
  // objects, and some callers compare them by identity: the rows are rebuilt.
  if (S.projects !== lastProjects || S.groups !== lastGroups) {
    lastProjects = S.projects;
    lastGroups = S.groups;
    listGen++;
  }
  const rows = [...sessions.values()].map((s) => {
    const t = ownTheme(s);
    return [
      s.id, s.state, s.unread, s.exit_code, s.agent_exit, s.started_at,
      taskTitle(s), agentName(s), agentKind(s), t ? ink(signatureColor(t)) : null,
      place(s)?.worktree.path ?? null, awayPlaces(s).map((a) => a.worktree.path),
    ];
  });
  const groups = S.groups.map((g) => {
    const lead = sessions.get(g.focus ?? filledOf(g.layout)[0]);
    return [g.id, g.name, g.layout, g.cwd, groupColor(g), accent(lead ? place(lead)?.project : null)];
  });
  const projects = S.projects.map((p) => [
    p.name, p.short, p.icon, p.fleet ?? null, p.error, p.default_branch, accent(p), selectedWorktree.get(p.name),
    p.worktrees.map((w) => [
      w.path, w.branch, w.is_main, w.locked, w.prunable, collapsed.has(w.path), removing.has(w.path), removeErrors.get(w.path),
      showsChanges(p) ? changesSig(w) : null, showsFiles(p) ? filesSig(w) : null,
    ]),
  ]);
  // The agent icons are inked against the applied app theme: a new theme re-inks them.
  return JSON.stringify([
    listGen, appliedTheme(), S.selectedProject, [...folded],
    S.selection, S.renamingSession, S.renaming?.id ?? null, rows, groups, projects,
  ]);
}

export function renderSidebar() {
  const side = $<HTMLElement>("sidebar-scroll");
  openToFocused();
  const sig = sidebarSig();
  if (side.dataset.sig === sig) return;
  side.dataset.sig = sig;
  // Keep focus and the selected range in an inline control across re-renders,
  // so a render while a rename is open does not undo its select-all.
  const active = document.activeElement as HTMLElement | null;
  // The sidebar is rebuilt whenever sessions update. Keep focus on any list
  // row as well as inline rename fields, otherwise a selected session loses
  // its keyboard shortcuts after the next render.
  const activeKey = active?.dataset?.key ?? active?.dataset?.session ?? active?.dataset?.wt ?? active?.dataset?.group;
  const caret: Caret | null = active instanceof HTMLInputElement && active.selectionStart !== null
    ? [active.selectionStart, active.selectionEnd ?? active.selectionStart, active.selectionDirection ?? "none"]
    : null;
  // The rows after the focused one, then the rows before it, nearest first. If the
  // focused row goes (its session ended), the cursor moves to the next one left.
  const items = listItems();
  const at = active ? items.indexOf(active) : -1;
  const near = at < 0 ? [] : [...items.slice(at + 1), ...items.slice(0, at).reverse()].map(itemKey);

  side.replaceChildren();
  for (const sec of treeSections()) {
    side.appendChild(sectionHead(sec));
    if (sec.fleet && !isOpen(fleetKey(sec.fleet))) continue;
    for (const p of sec.projects) side.appendChild(projectBlock(p));
  }
  if (!S.projects.length) side.appendChild(h("div", "note-row", "No projects"));
  // Sessions whose folder is in no project.
  const other = worktreeLines(null);
  if (other.length) {
    const head = h("div", "tree-head");
    head.append(h("span", "tree-head-name", "Other"));
    side.append(head, sessionBlock(other, DEFAULT_ACCENT));
  }
  restoreFocus(side, activeKey, caret, near);
}

/** A section head: "Projects" with its +, or a fleet that folds and opens. */
function sectionHead(sec: Section): HTMLElement {
  if (!sec.fleet) {
    const head = h("div", "tree-head");
    head.append(h("span", "tree-head-name", "Projects"));
    const add = button("tree-head-add", "", () => void addProject.open());
    add.title = "Add project";
    add.setAttribute("aria-label", "Add project");
    add.appendChild(plusIcon());
    head.appendChild(add);
    return head;
  }
  const name = sec.fleet;
  const key = fleetKey(name);
  const open = isOpen(key);
  const head = button("tree-head fleet-pick", "", () => {
    setOpen(key, !open);
    render();
  });
  head.dataset.key = key;
  head.setAttribute("aria-expanded", String(open));
  head.append(chevron(open), h("span", "tree-head-name", name));
  const roll = open ? null : rollup(sec.projects);
  if (roll) head.appendChild(roll);
  head.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    fleetMenu({ name, projects: sec.projects }, e.clientX, e.clientY);
  });
  return head;
}

/** A worktree that needs no row of its own: the only one, on its usual branch. Its sessions sit under the project. */
function bareWorktree(p: Project): Worktree | null {
  if (p.worktrees.length !== 1) return null;
  const w = p.worktrees[0];
  const usual = p.default_branch ? w.branch === p.default_branch : ["main", "master"].includes(w.branch ?? "");
  return w.is_main && usual && !w.locked && !w.prunable && !removing.has(w.path) && !removeErrors.has(w.path) ? w : null;
}

/** A project: one 40px row, then its worktrees, groups and sessions while it is open. */
function projectBlock(p: Project): HTMLElement {
  const color = accent(p);
  const key = projectKey(p.name);
  const open = isOpen(key);
  const bare = bareWorktree(p);
  const block = h("div", "tree-project" + (p.name === S.selectedProject ? " selected" : ""));
  block.style.setProperty("--pc", color);

  const head = h("div", "project-row");
  const pick = h("button", "project-pick");
  pick.type = "button";
  pick.dataset.key = key;
  pick.tabIndex = -1;
  pick.setAttribute("aria-expanded", String(open));
  const fold = h("span", "tree-fold");
  fold.appendChild(chevron(open));
  fold.addEventListener("click", (e) => {
    e.stopPropagation();
    setOpen(key, !open);
    render();
  });
  pick.append(fold, projectBadge(p), h("span", "project-name", p.name));
  if (p.error) pick.appendChild(h("span", "project-meta", "error"));
  const roll = open ? null : rollup([p]);
  if (roll) pick.appendChild(roll);
  pick.addEventListener("click", () => selectProject(p.name));
  head.appendChild(pick);
  if (bare) {
    const plus = button("wt-plus", "", () => launchMenu.open(plus, p, bare));
    plus.dataset.wt = bare.path;
    plus.setAttribute("aria-label", `Start a session in ${p.name}`);
    plus.setAttribute("aria-haspopup", "menu");
    plus.appendChild(plusIcon());
    head.appendChild(plus);
  }
  const more = button("wt-plus", "", () => {
    const r = more.getBoundingClientRect();
    projectMenu(p, r.right, r.top);
  });
  more.setAttribute("aria-label", `${p.name} menu`);
  more.setAttribute("aria-haspopup", "menu");
  more.appendChild(icon('<circle cx="5" cy="12" r="1"></circle><circle cx="12" cy="12" r="1"></circle><circle cx="19" cy="12" r="1"></circle>'));
  head.appendChild(more);
  head.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    projectMenu(p, e.clientX, e.clientY);
  });
  block.appendChild(head);
  if (!open) return block;

  if (p.error) block.appendChild(errorRow(p.error));
  else if (p.worktrees.length === 0) block.appendChild(h("div", "note-row", "No worktrees"));
  else if (bare) {
    const body = h("div", "wt-body tree-bare");
    body.appendChild(sessionBlock(worktreeLines(bare), color));
    const changes = showsChanges(p) ? changesBlock(bare) : null;
    if (changes) body.appendChild(changes);
    if (showsFiles(p)) body.appendChild(filesBlock(bare));
    block.appendChild(body);
  } else {
    const sel = selectedWorktree.get(p.name);
    for (const w of p.worktrees) block.appendChild(worktreeBlock(p, w, w.path === sel, color));
  }
  return block;
}

type Caret = [start: number, end: number, dir: "forward" | "backward" | "none"];

/** Where the pointer last was over the list, or null when it left. */
let pointer: [number, number] | null = null;
const HOVERABLE = ".session-row, .wt-pick, .wt-plus, .wt-count, .group-pick, .file-row";
{
  const side = $<HTMLElement>("sidebar-scroll");
  side.addEventListener("mousemove", (e) => (pointer = [e.clientX, e.clientY]));
  side.addEventListener("mouseleave", () => (pointer = null));
  side.addEventListener("mouseout", (e) => (e.target as HTMLElement).closest?.(".hover")?.classList.remove("hover"));
}

/**
 * A rebuilt row has no :hover until the pointer moves, so the row under a
 * still pointer flashes on every render. The class stands in until it moves.
 */
function restoreHover() {
  if (!pointer) return;
  document.elementFromPoint(...pointer)?.closest(HOVERABLE)?.classList.add("hover");
}

function restoreFocus(side: HTMLElement, activeKey: string | undefined, caret: Caret | null, near: string[]) {
  restoreHover();
  if (activeKey) {
    const again = [...side.querySelectorAll<HTMLElement>("[data-key], [data-session], [data-wt], [data-group]")]
      .find((el) => (el.dataset.key ?? el.dataset.session ?? el.dataset.wt ?? el.dataset.group) === activeKey);
    const disabled = again instanceof HTMLButtonElement || again instanceof HTMLInputElement || again instanceof HTMLSelectElement
      ? again.disabled
      : false;
    if (again && !disabled) {
      again.focus();
      if (caret !== null && again instanceof HTMLInputElement) again.setSelectionRange(...caret);
    } else if (!again) {
      const keys = new Set(listItems().map(itemKey));
      const next = near.find((k) => keys.has(k));
      if (next) {
        keepRow(next);
        // The row that takes the place of the current one shows its preview, or the panes.
        startPreview(next, false);
      }
    }
  }
}

/** One color per group, by creation order, so a group keeps its color. */
const GROUP_COLORS = ["#9ec1ff", "#f28fd0", "#7ee0cb", "#e0c07e", "#ff9e7a", "#c3e88d"];

const groupColor = (g: Group) => ink(GROUP_COLORS[Math.max(0, S.groups.indexOf(g)) % GROUP_COLORS.length]);

function groupRow(g: Group, hasPrevious = false, hasNext = false): HTMLElement {
  const ids = filledOf(g.layout).filter((id) => sessions.has(id));
  const row = h("div", "group-row" + (hasPrevious ? " has-previous" : "") + (hasNext ? " has-next" : "") );
  row.dataset.group = g.id;
  row.style.setProperty("--gc", groupColor(g));
  const lead = sessions.get(g.focus ?? ids[0]);
  row.style.setProperty("--pc", accent(lead ? place(lead)?.project : null));
  if (S.renaming?.id === g.id) {
    const input = groupRenameInput(g);
    row.append(glyph(g.layout), input);
    return row;
  }
  const pick = h("button", "group-pick");
  pick.dataset.group = g.id;
  pick.type = "button";
  pick.append(glyph(g.layout), h("span", "group-name", g.name));
  pick.addEventListener("click", (e) => {
    if (e.ctrlKey || e.metaKey) return toggleSelectGroup(g);
    // detail counts clicks across the re-render the first click causes.
    if (e.detail >= 2) startRename(g);
    else if (g.id !== S.activeGroup || g.id !== S.groupPicked) showGroup(g.id, true);
  });
  pick.addEventListener("dblclick", () => startRename(g));
  pick.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    groupMenu(g, e.clientX, e.clientY);
  });
  row.appendChild(pick);
  return row;
}

function worktreeBlock(p: Project, w: Worktree, selected: boolean, color: string): HTMLElement {
  const block = h("div", "wt" + (selected ? " selected" : ""));
  block.style.setProperty("--pc", color);

  const open = !collapsed.has(w.path);

  const row = h("div", "wt-row");
  const pick = button("wt-pick", "", () => {
    if (open) collapsed.add(w.path);
    else collapsed.delete(w.path);
    selectWorktree(p, w);
  });
  pick.dataset.wt = w.path;
  pick.setAttribute("aria-expanded", String(open));
  pick.append(chevron(open), branchIcon(), h("span", "branch", branchName(w)));
  // The main checkout shows its tag only while it is off its usual branch.
  const usual = p.default_branch ? w.branch === p.default_branch : ["main", "master"].includes(w.branch ?? "");
  if (w.is_main && !usual) pick.appendChild(h("span", "tag", "main folder"));
  if (w.locked) pick.appendChild(h("span", "tag", "locked"));
  if (w.prunable) pick.appendChild(h("span", "tag", "prunable"));
  if (removing.has(w.path)) pick.appendChild(h("span", "tag", "removing…"));
  // The rows under an open worktree show the detail.
  const diff = showsChanges(p) && !open ? changeCounts(w) : null;
  if (diff) pick.appendChild(diff);
  row.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    worktreeMenu(p, w, e.clientX, e.clientY);
  });
  row.appendChild(pick);
  const plus = button("wt-plus", "", () => launchMenu.open(plus, p, w));
  plus.dataset.wt = w.path;
  plus.setAttribute("aria-label", `Start a session in ${branchName(w)}`);
  plus.setAttribute("aria-haspopup", "menu");
  plus.appendChild(plusIcon());
  row.appendChild(plus);
  block.appendChild(row);

  if (open) {
    // No count line: the rows carry their own marks, and a line that comes
    // and goes would move the whole tree.
    const body = h("div", "wt-body");
    body.appendChild(sessionBlock(worktreeLines(w), color));
    const changes = showsChanges(p) ? changesBlock(w) : null;
    if (changes) body.appendChild(changes);
    if (showsFiles(p)) body.appendChild(filesBlock(w));
    block.appendChild(body);
  }

  const err = removeErrors.get(w.path);
  if (err) block.appendChild(errorRow(err));
  return block;
}

// Right-click on the empty part of the list: the + menu for the current worktree.
$<HTMLElement>("sidebar-scroll").addEventListener("contextmenu", (e) => {
  if (e.defaultPrevented || (e.target as Element).closest("button, input, .wt-count")) return;
  const at = currentWorktree();
  if (!at) return;
  e.preventDefault();
  launchMenu.open({ x: e.clientX, y: e.clientY }, at.p, at.w);
});
