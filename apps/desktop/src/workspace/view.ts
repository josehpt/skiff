/** What the terminal area shows and which pane has the keys. */
import { isSkiffAgent } from "../app/skiffAgent";
import { beginDrag, type DropTarget } from "./drag";
import { build, type Direction, leaf, neighbor, removePane, replacePane, sessionsOf, shape, splitPane } from "./layout";

import { branchName, bySessionPriority, isUnread, locate, taskTitle } from "./model";
import type { Group, Layout, Project, SplitDir, Worktree } from "../platform/types";
import { focusFirstSlot } from "./canvas";
import { filledOf, isSlot, slot, slotsOf } from "./layoutSlots";
import { host } from "../terminal/terminalHost";
import { modalOpen } from "../ui/dom";
import { showError } from "../ui/alerts";
import { autoName, deleteGroup, saveGroup } from "../app/groups";
import { render } from "../app/render";
import { clearSelection } from "./selection";
import { FULL_HINT, MAX_PANES, OTHER, S, selectedWorktree, sessions } from "../app/state";
import { activeGroupObj, currentLayout, groupOf, inShownGroup, place, shownIds, splitFull, worktreeSessions } from "../app/stateQueries";
import { panes } from "../terminal/terminalState";
import { previewOnScreen } from "../terminal/peek";
import { currentRow, listItems } from "../app/keyboard";
import { worktreeRowKey } from "./tree";
import { view } from "../terminal/terminal";

let background = 0;
let held = 0;

/**
 * Pane changes that come from skiffd, not from the user: a session ends, the
 * groups reload. While a preview shows, they change the panes under it and
 * leave the current row and the keys alone.
 */
export function inBackground(fn: () => void, always = false) {
  background++;
  if (always) held++;
  try {
    fn();
  } finally {
    background--;
    if (always) held--;
  }
}

/** `always`: it never takes the row or the keys, preview or not. */
const quiet = () => held > 0 || (background > 0 && previewOnScreen());

/** Give the keys to a shown pane. */
export function focusPane(id: string, grab = true) {
  const background = quiet();
  if (background) grab = false;
  S.justAdded = null;
  S.grab = null;
  if (grab) {
    S.groupPicked = null;
    // The keys go to this pane, so its row is where we are.
    S.roveKey = id;
  }
  if (S.focused && S.focused !== id) S.previous = S.focused;
  S.focused = id;
  const g = activeGroupObj();
  if (g) g.focus = id;
  const s = sessions.get(id);
  const at = s ? place(s) : null;
  // A change in the background keeps the sidebar where the user is.
  if (background) {
    // The project and worktree stay as they are.
  } else if (at) {
    S.selectedProject = at.project.name;
    selectedWorktree.set(at.project.name, at.worktree.path);
  } else if (s && !isSkiffAgent(id)) {
    S.selectedProject = OTHER;
  }
  // The Skiff agent is in no project: the project stays as it was, so the
  // tree does not fall back to the first project and light it.
  render();
  requestAnimationFrame(() => {
    // A right-click focuses the pane, then opens a menu that keeps the keys.
    // A text field (a group name being typed) keeps them too.
    const a = document.activeElement;
    const typing = a instanceof HTMLInputElement && !host.contains(a);
    // A preview over the panes keeps the keys. Leaving it ends it first.
    if (modalOpen() || typing || S.focused !== id || !grab || S.atRail || previewOnScreen()) return;
    const pane = panes.get(id);
    if (pane?.term.element) pane.term.focus();
    // Not open yet: the terminal takes the keys when it attaches.
    else S.grab = id;
  });
}

export function refocusTerminal() {
  // A menu opened from the rail gives the keys back to the rail.
  if (S.atRail) document.querySelector<HTMLElement>("#rail .rail-chip.active")?.focus();
  // A preview covers the panes: the keys go back to its row, not to a pane under it.
  else if (previewOnScreen()) currentRow()?.focus();
  else if (S.focused) panes.get(S.focused)?.term.focus();
}

/** One session fills the area. Any group stays in the sidebar. */
export function showSingle(id: string) {
  S.activeGroup = null;
  S.single = id;
  focusPane(id);
}

/** Show a group. `pick` selects the group itself: the keys stay on its row, not in a terminal. */
export function showGroup(id: string, pick = false, focus?: string) {
  const g = S.groups.find((x) => x.id === id);
  if (!g) return;
  S.activeGroup = id;
  S.single = null;
  const ids = sessionsOf(g.layout).filter((x) => !isSlot(x));
  if (pick) {
    S.groupPicked = id;
    S.roveKey = id;
  }
  const f = focus && ids.includes(focus) ? focus : g.focus && ids.includes(g.focus) ? g.focus : ids[0];
  if (f) focusPane(f, !pick);
  else {
    // A canvas with every pane empty: the keys go to its first pane's list,
    // even when `pick` asked to keep them on the group row. There is no
    // session yet for the row-keeps-the-keys trick (passToTerminal) to
    // forward typing into, so the picker is the only way in.
    S.focused = null;
    render();
    focusFirstSlot();
    return;
  }
  if (pick) {
    requestAnimationFrame(() => document.querySelector<HTMLElement>(`#sidebar-scroll .group-pick[data-group="${id}"]`)?.focus());
  }
}

/**
 * Show a new tree. Two or more panes are a group: the active one is updated,
 * else a new one is made. One pane stays in the active group; without one it is a single view.
 */
export function applyLayout(next: Layout | null, focus: string | null) {
  const ids = sessionsOf(next);
  const g = activeGroupObj();
  const f = focus && ids.includes(focus) ? focus : ids[0] ?? null;
  if (next && (ids.length >= 2 || g)) {
    S.single = null;
    let target = g;
    if (target) {
      target.layout = next;
      target.focus = f;
      saveGroup(target);
    } else {
      target = { id: `tmp-${++S.tmpSeq}`, name: autoName(ids), layout: next, focus: f };
      S.groups.push(target);
      S.activeGroup = target.id;
      saveGroup(target);
    }
    releaseFromOtherGroups(target, ids);
  } else {
    if (g) {
      // The group keeps its slot when its last session leaves.
      g.layout = slot();
      g.focus = null;
      saveGroup(g);
      S.single = null;
      render();
      return focusFirstSlot();
    }
    S.activeGroup = null;
    S.single = f;
  }
  if (f) focusPane(f);
  else unfocus();
}

/** A session belongs to one group: take `ids` out of every other group. */
function releaseFromOtherGroups(keep: Group, ids: string[]) {
  for (const other of [...S.groups]) {
    if (other === keep) continue;
    let layout: Layout | null = other.layout;
    for (const id of ids) layout = removePane(layout, id);
    if (layout === other.layout || sessionsOf(layout).join() === sessionsOf(other.layout).join()) continue;
    // Its last session left: the group stays, with an empty pane.
    other.layout = layout ?? slot();
    if (other.focus && !sessionsOf(other.layout).includes(other.focus)) other.focus = sessionsOf(other.layout).find((x) => !isSlot(x)) ?? null;
    saveGroup(other);
  }
}

/** Take a session out of its group. */
export function removeFromGroup(id: string) {
  const g = groupOf(id);
  if (!g) return;
  if (g.id === S.activeGroup) {
    // The session pops out of the group and stays selected, alone.
    g.layout = emptyLast(g, id) ?? removePane(g.layout, id) ?? slot();
    g.focus = sessionsOf(g.layout).find((x) => !isSlot(x)) ?? null;
    saveGroup(g);
    return showSingle(id);
  }
  const kept = emptyLast(g, id);
  if (kept) {
    g.layout = kept;
    g.focus = null;
    saveGroup(g);
    return render();
  }
  const rest = removePane(g.layout, id);
  if (!rest) deleteGroup(g);
  else {
    g.layout = rest;
    if (g.focus === id) g.focus = sessionsOf(rest)[0];
    saveGroup(g);
  }
  render();
}

/** Put `id` next to `target`. A session shown elsewhere in the layout moves. */
export function splitWith(target: string | null, dir: SplitDir, id: string) {
  if (!shownIds().includes(id) && splitFull()) return;
  let base = currentLayout();
  if (sessionsOf(base).includes(id)) base = removePane(base, id);
  if (!base) return showSingle(id);
  const ids = sessionsOf(base);
  const t = target && ids.includes(target) ? target : ids[0];
  applyLayout(splitPane(base, t, dir, id), id);
}

/** Click on a session: focus it where it is shown, else show its group, else show it alone. A click never changes a group. */
export function revealSession(id: string) {
  if (shownIds().includes(id)) return focusPane(id);
  const g = S.groups.find((x) => sessionsOf(x.layout).includes(id));
  // One render: the group shows with this pane focused.
  if (g) showGroup(g.id, false, id);
  else showSingle(id);
}

/** The group's layout with `id` swapped for an empty pane, when it was the last session: the group stays open. */
function emptyLast(g: Group, id: string): Layout | null {
  const rest = removePane(g.layout, id);
  return filledOf(rest).length === 0 ? replacePane(g.layout, id, slot) : null;
}

/**
 * The pane's × and the close key: the session leaves the group on screen and
 * stays selected, alone. A single pane has no group to leave: nothing happens.
 */
export function closePane(id: string) {
  if (inShownGroup(id)) removeFromGroup(id);
}

/** The pane beside `id` that takes the keys when `id` ends: left, right, up, then down. */
export function paneNear(id: string): string | null {
  const rects = view.cellRects();
  for (const dir of ["left", "right", "up", "down"] as const) {
    const n = neighbor(rects, id, dir);
    if (n && !isSlot(n)) return n;
  }
  return null;
}

/** The session row after `id` in the list, else the one before it. */
export function rowNear(id: string): string | null {
  const rows = listItems().filter((el) => el.dataset.session);
  const at = rows.findIndex((el) => el.dataset.session === id);
  if (at < 0) return null;
  const near = [...rows.slice(at + 1), ...rows.slice(0, at).reverse()];
  return near.map((el) => el.dataset.session!).find((x) => x !== id && sessions.has(x)) ?? null;
}

/** Keep the focused pane as a single view and drop the group. Sessions keep running. */
export function unsplit(g: Group) {
  const ids = sessionsOf(g.layout).filter((x) => !isSlot(x));
  const f = g.id === S.activeGroup && S.focused ? S.focused : g.focus && ids.includes(g.focus) ? g.focus : ids[0];
  const active = g.id === S.activeGroup;
  deleteGroup(g);
  if (f) showSingle(f);
  else if (active) unfocus();
  else render();
}

export function moveFocus(dir: Direction) {
  if (!S.focused) return;
  const next = neighbor(view.cellRects(), S.focused, dir);
  if (next) focusPane(next);
}

/**
 * The layout a drop on a pane makes. An edge splits that pane there; the
 * center replaces it (its session keeps running). Several sessions land
 * as one block, tiled like Open in split. Null: the drop changes nothing.
 */
function dropLayout(t: DropTarget, ids: string[]): Layout | null {
  if (ids.length === 1 && t.session === ids[0]) return null;
  const now = currentLayout();
  let base = now;
  for (const id of ids) base = removePane(base, id);
  if (sessionsOf(base).length + ids.length > MAX_PANES) return null;
  const block = build(ids);
  const target = t.session && sessionsOf(base).includes(t.session) ? t.session : null;
  let next: Layout;
  // The rim of the area: wrap every pane, so one pane spans the full width or height (a T shape).
  if (t.outer && base) {
    const dir: SplitDir = t.zone === "left" || t.zone === "right" ? "row" : "col";
    const before = t.zone === "left" || t.zone === "top";
    next = { type: "split", dir, ratio: 0.5, a: before ? block : base, b: before ? base : block };
  }
  // Every shown pane was dragged, or nothing was shown: the block is the view.
  else if (!base) next = block;
  // The target pane was itself dragged away: put the block beside the rest.
  else if (!target) next = { type: "split", dir: "row", ratio: 0.5, a: base, b: block };
  else if (t.zone === "center") next = replacePane(base, target, () => block);
  else {
    const dir: SplitDir = t.zone === "left" || t.zone === "right" ? "row" : "col";
    const before = t.zone === "left" || t.zone === "top";
    next = replacePane(base, target, (p) => ({ type: "split", dir, ratio: 0.5, a: before ? block : p, b: before ? p : block }));
  }
  // A drop that puts the panes back where they are is no drop.
  return shape(next) === shape(now) ? null : next;
}

/** A group row takes a drop only when it gains a session. */
function groupGains(groupId: string, ids: string[]) {
  const g = S.groups.find((x) => x.id === groupId);
  return !!g && ids.some((id) => !sessionsOf(g.layout).includes(id));
}

function dropOn(t: DropTarget, dragged: string[]) {
  const ids = dragged.filter((id) => sessions.has(id));
  if (!ids.length) return;
  if (t.group) return addToGroup(t.group, ids);
  const next = dropLayout(t, ids);
  if (!next) return;
  clearSelection();
  applyLayout(next, ids[0]);
}

/** Drop sessions on a group row: they fill its empty panes, then split in. A full group takes no more. */
function addToGroup(groupId: string, dragged: string[]) {
  const g = S.groups.find((x) => x.id === groupId);
  if (!g) return;
  const ids = dragged.filter((id) => !sessionsOf(g.layout).includes(id));
  if (!ids.length) return;
  const room = MAX_PANES - sessionsOf(g.layout).length + slotsOf(g.layout).length;
  if (ids.length > room) return showError(`The group is full (${MAX_PANES} panes).`);
  releaseFromOtherGroups(g, ids);
  let layout = g.layout;
  for (const id of ids) {
    const slot = slotsOf(layout)[0];
    if (slot) layout = replacePane(layout, slot, () => leaf(id));
    else if (sessionsOf(layout).length === 2) layout = splitPane(layout, sessionsOf(layout)[1], "col", id);
    else layout = build([...sessionsOf(layout), id]);
  }
  g.layout = layout;
  if (S.single && ids.includes(S.single)) S.single = null;
  clearSelection();
  saveGroup(g);
  showGroup(g.id, false, ids[0]);
}

/** Each group's drop area: its header row and its nested session rows, as one box. */
function groupRects() {
  const boxes = new Map<string, DOMRect>();
  for (const el of document.querySelectorAll<HTMLElement>("#sidebar-scroll .group-row, #sidebar-scroll .session-row[data-in-group]")) {
    const id = el.dataset.group ?? el.dataset.inGroup;
    if (!id) continue;
    const r = el.getBoundingClientRect();
    const b = boxes.get(id);
    boxes.set(id, b ? new DOMRect(Math.min(b.left, r.left), Math.min(b.top, r.top), Math.max(b.right, r.right) - Math.min(b.left, r.left), Math.max(b.bottom, r.bottom) - Math.min(b.top, r.top)) : r);
  }
  return [...boxes].map(([id, rect]) => ({ id, rect }));
}

/** A group lives in the worktree of its first session. A canvas with no session yet lives where it was made. */
export function groupHome(g: Group): Worktree | null {
  const first = sessions.get(filledOf(g.layout)[0]);
  if (first) return place(first)?.worktree ?? null;
  return g.cwd ? locate(S.projects, g.cwd)?.worktree ?? null : null;
}

const home = (id: string) => {
  const s = sessions.get(id);
  return s ? place(s)?.worktree.path ?? null : null;
};

/**
 * Why a drop would mix worktrees, or null. A session's process runs where it
 * started, so a pane or group never shows a session from another worktree.
 * Undefined: the target has no worktree yet, so any session fits.
 */
function mixesWorktrees(t: DropTarget, ids: string[]): string | null {
  let target: string | null | undefined;
  if (t.group) {
    const g = S.groups.find((x) => x.id === t.group);
    target = g ? groupHome(g)?.path ?? (filledOf(g.layout).length ? null : undefined) : undefined;
  } else {
    const other = t.session ?? shownIds().find((id) => !ids.includes(id));
    target = other ? home(other) : undefined;
  }
  if (target === undefined) return null;
  const off = ids.find((id) => home(id) !== target);
  if (!off) return null;
  const s = sessions.get(off);
  const at = s ? place(s) : null;
  return `Runs in ${at ? branchName(at.worktree) : "another folder"}`;
}

/** Start a drag of these sessions; a press that does not move stays a click. */
export function dragSessions(e: MouseEvent, ids: string[]) {
  const first = sessions.get(ids[0]);
  const label = ids.length > 1 ? `${ids.length} sessions` : first ? taskTitle(first) : "session";
  // A full split takes no more panes; moving a pane inside it still works.
  const blocked = splitFull() && ids.some((id) => !shownIds().includes(id));
  // A dragged pane is no target for itself. When it is the whole view, the rim
  // and the empty area would put it back where it is, so they are off too.
  const alone = shownIds().length > 0 && shownIds().every((id) => ids.includes(id));
  const cells = () => {
    if (blocked) return new Map<string, DOMRect>();
    const m = view.cellRects();
    for (const id of ids) m.delete(id);
    return m;
  };
  beginDrag(e, {
    label: blocked ? `${label}: ${FULL_HINT}` : label,
    cells,
    area: blocked || alone ? null : host,
    groupRows: groupRects,
    refuses: (t) => mixesWorktrees(t, ids),
    accepts: (t) => (t.group ? groupGains(t.group, ids) : !!dropLayout(t, ids)),
    drop: (t) => dropOn(t, ids),
  });
}

/**
 * The terminal area follows the selection. A shown split that already holds
 * a session of this worktree stays. Else the worktree's best session shows,
 * in its group if it has one. No session: nothing has the keys.
 */
/** The session last focused in each worktree, by path. A return to the worktree opens it again. */
const lastFocus = new Map<string, string>();

/** Notes where the keys are before a project or worktree switch moves them. */
function noteFocus() {
  const s = S.focused ? sessions.get(S.focused) : undefined;
  const at = s ? place(s) : null;
  if (s && at) lastFocus.set(at.worktree.path, s.id);
}

export function selectWorktree(project: Project, w: Worktree) {
  noteFocus();
  S.selectedProject = project.name;
  selectedWorktree.set(project.name, w.path);
  // The worktree's row is where we are, until a session it shows takes the row.
  S.roveKey = worktreeRowKey(project, w);
  S.groupPicked = null;
  requestAnimationFrame(() => currentRow()?.scrollIntoView({ block: "nearest" }));
  const here = worktreeSessions(w);
  if (here.some((x) => shownIds().includes(x.id))) return render();
  const last = lastFocus.get(w.path);
  if (last && here.some((x) => x.id === last)) return revealSession(last);
  const best = here.sort(bySessionPriority)[0];
  if (best) revealSession(best.id);
  else unfocus();
}

/** No session has the keys: hide every pane and offer to start one here. */
export function unfocus() {
  S.focused = null;
  S.single = null;
  S.activeGroup = null;
  const keep = quiet();
  if (!keep) (document.activeElement as HTMLElement | null)?.blur();
  render();
  if (!S.atRail && !previewOnScreen()) host.querySelector<HTMLButtonElement>(".welcome button")?.focus();
}

export function selectProject(name: string) {
  const p = S.projects.find((x) => x.name === name);
  if (!p) return;
  noteFocus();
  S.selectedProject = name;
  const path = selectedWorktree.get(name);
  const w = p.worktrees.find((x) => x.path === path) ?? p.worktrees.find((x) => x.is_main) ?? p.worktrees[0];
  if (w) selectWorktree(p, w);
  else unfocus();
}

/** The next session that waits or has unread output. */
export function nextWaiting(): string | undefined {
  // Bells first, then unread results: the sort ranks waiting ahead of unread.
  const waiting = [...sessions.values()].filter((s) => s.state === "waiting" || isUnread(s)).sort(bySessionPriority);
  return (waiting.find((s) => s.id !== S.focused) ?? waiting[0])?.id;
}

export function nextUnread(): string | undefined {
  const unread = [...sessions.values()].filter(isUnread).sort(bySessionPriority);
  return (unread.find((s) => s.id !== S.focused) ?? unread[0])?.id;
}

export function goBack() {
  if (S.previous && sessions.has(S.previous)) revealSession(S.previous);
}
