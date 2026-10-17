/**
 * Mission control (#88): every session's screen over the whole window, one
 * lane per fleet. Read-only text from skiffd; no pane, no PTY resize.
 * Ctrl+Shift+A or Esc closes it. A click or Enter goes to the session.
 * Ctrl+Shift+P searches the sessions on the board, and hides the rest. The
 * search stays until Esc clears it, so a close and an open keep it.
 * Running / All at the right of the search (#133): Running hides idle
 * sessions and shells at their prompt. Each part counts what the search
 * leaves. The choice is stored in `[appearance] mission_filter`, so the board
 * opens with the last one, also after a restart. With none stored: Running.
 * The Skipper lane (missionSkipper.ts) comes first whenever Skipper has a
 * folder: its open tasks, then its own screen.
 * This file is the board. missionMotion.ts opens and closes it.
 */

import { invoke } from "@tauri-apps/api/core";

import type { Project, SessionInfo } from "../platform/types";
import { agentIcon, agentKind } from "../appearance/agentIcon";
import { stateIcon } from "../appearance/stateIcon";
import { DEFAULT_THEME, isLight } from "../appearance/colors";
import { appThemeOf, themeIdFor, xtermTheme } from "../appearance/themes";
import { appThemeVars } from "../appearance/appTheme";
import { type Action, actionFor, keyLabel } from "../app/keys";
import { S } from "../app/state";
import { accent, place } from "../app/stateQueries";
import { h } from "../ui/dom";
import { agentName, branchName, fuzzy, taskTitle } from "./model";
import { worktreeLines } from "./sidebarLines";
import { bareWorktree, projectBadge, rollup, treeSections } from "./tree";
import { closeMission, goToSession, maximizeLit } from "./missionMotion";
import { nextWaiting } from "./view";
import { settings } from "../app/panels";
import {
  type SkipperView,
  freshSkipper,
  isSkipperKey,
  openUid,
  skipper,
  skipperEnter,
  skipperKeys,
  skipperLane,
  skipperMatch,
  skipperSession,
  skipperSignature,
  skipperView,
  taskMenu,
} from "./missionSkipper";

/** Rows of text per screen. */
const ROWS = 12;
/** How often the open board reads the screens again. */
const TICK = 1000;

interface Lane {
  name: string;
  projects: { p: Project; list: SessionInfo[] }[];
}

const root = h("div", "mission");
root.id = "mission-control";
root.hidden = true;
root.tabIndex = -1;
root.setAttribute("role", "dialog");
root.setAttribute("aria-label", "Mission control");
document.body.appendChild(root);

let lit: string | null = null;
let order: string[][] = [];
/** The Skipper lane as the last build or update saw it. Null: no lane. */
let sk: SkipperView | null = null;
let shape = "";
let timer: number | undefined;
let reading = false;
// Set by board.stop(): no rebuild, update or read until board.start().
let paused = false;
const screens = new Map<string, HTMLElement>();

/**
 * The search, in the app's top bar where the palette field sits; it shows
 * only while the board is open. `found`: the sessions it matches; null with no search.
 */
const search = h("input", "mission-search");
search.type = "text";
search.autocomplete = "off";
search.spellcheck = false;
search.placeholder = "Search sessions";
search.setAttribute("aria-label", "Search sessions");
const find = h("label", "tb-cell mission-find");
find.appendChild(search);
document.getElementById("open-switcher")?.after(find);
const noMatch = h("div", "mission-empty", "No match");
let found: Set<string> | null = null;

/** Running / All, at the right of the search. `running`: only sessions that work or wait. */
let running = true;
const scope = h("div", "mission-scope");
scope.setAttribute("role", "group");
scope.setAttribute("aria-label", "Sessions to show");
const allCount = h("span", "mission-scope-n");
const runningCount = h("span", "mission-scope-n");
const scopeAll = h("button", "mission-scope-btn");
scopeAll.append("All", allCount);
const scopeRunning = h("button", "mission-scope-btn");
scopeRunning.append(h("span", "mission-scope-dot"), "Running", runningCount);
for (const [b, on] of [[scopeRunning, true], [scopeAll, false]] as const) {
  b.type = "button";
  b.tabIndex = -1;
  // The keys stay where they are: on the board or in the search.
  b.addEventListener("mousedown", (e) => e.preventDefault());
  b.addEventListener("click", (e) => {
    // Inside the search's label: the click must not go on to the search.
    e.preventDefault();
    setRunning(on, true);
  });
  scope.appendChild(b);
}
find.appendChild(scope);

/** A session that works or waits. An idle session and a shell at its prompt are not. */
const isRunning = (s: SessionInfo) => (s.state === "working" || s.state === "waiting") && agentKind(s) !== "shell";

/** Shows Running or All. `store`: a change by the user, kept in the config. */
function setRunning(on: boolean, store = false) {
  if (store && on !== running) invoke("set_mission_filter", { running: on }).catch(console.error);
  running = on;
  scopeAll.setAttribute("aria-pressed", String(!on));
  scopeRunning.setAttribute("aria-pressed", String(on));
  if (!root.hidden) filter(lanes());
}
setRunning(true);

/** The stored filter, from `[appearance] mission_filter` when the app starts. Only `all` shows all. */
export function restoreRunning(filter: string | null | undefined) {
  setRunning(filter !== "all");
}

/** The Running key here: only the running sessions, or all. */
export function toggleRunning() {
  setRunning(!running, true);
}

/** A project's sessions in tree order: each worktree's groups and their members, then the rest. */
function projectSessions(p: Project): SessionInfo[] {
  const seen = new Set<string>();
  const out: SessionInfo[] = [];
  for (const w of p.worktrees) {
    for (const l of worktreeLines(w)) {
      if (!l.pointer && !l.group && !seen.has(l.session.id)) {
        seen.add(l.session.id);
        out.push(l.session);
      }
    }
  }
  return out;
}

/** What the search matches: project, branch, agent, title and state, as the palette does. */
function matchText(s: SessionInfo, p: Project): string {
  const w = place(s)?.worktree;
  return `${p.name} ${w ? branchName(w) : ""} ${agentName(s)} ${taskTitle(s)} ${s.state}`;
}

/**
 * Hides the screens the search and Running leave out, and the projects and
 * lanes left with none. Running leaves the Skipper lane as it is. The light
 * moves to the first match when its screen hides.
 */
function filter(all: Lane[], scroll = false) {
  const q = search.value.trim();
  found = q || running ? new Set() : null;
  // Each part of Running / All counts what the search leaves. Skipper's rows count in neither.
  let nAll = 0;
  let nRunning = 0;
  for (const l of all)
    for (const { p, list } of l.projects)
      for (const s of list) {
        if (q && fuzzy(q, matchText(s, p)) < 0) continue;
        const on = isRunning(s);
        nAll++;
        if (on) nRunning++;
        if (found && (on || !running)) found.add(s.id);
      }
  if (found && sk) for (const k of skipperKeys(sk)) if (!q || fuzzy(q, skipperMatch(sk, k)) >= 0) found.add(k);
  allCount.textContent = String(nAll);
  runningCount.textContent = String(nRunning);
  for (const t of root.querySelectorAll<HTMLElement>(".mission-tile")) t.hidden = !!found && !found.has(t.dataset.session ?? "");
  for (const el of root.querySelectorAll<HTMLElement>(".mission-group, .mission-lane")) el.hidden = !!found && !el.querySelector(".mission-tile:not([hidden])");
  const board = root.querySelector(".mission-lanes");
  if (found && !found.size) board?.appendChild(noMatch);
  else noMatch.remove();
  const first = shown().flat()[0];
  if (first && (scroll || !lit || (found && !found.has(lit)))) light(first, true);
  else if (scroll) root.querySelector<HTMLElement>(".mission-lanes")?.scrollTo({ left: 0 });
}

/** The sessions the arrows move through: each lane's, less those the search hides. */
function shown(): string[][] {
  return found ? order.map((l) => l.filter((id) => found!.has(id))) : order;
}

/**
 * Each fleet is a lane, as in the tree, but only a fleet with sessions (#107).
 * Projects with no sessions are left out too. The tree still shows both.
 */
function lanes(): Lane[] {
  return treeSections()
    .map((sec) => ({
      name: sec.fleet ?? "Projects",
      projects: sec.projects.map((p) => ({ p, list: projectSessions(p) })).filter((x) => x.list.length),
    }))
    .filter((l) => l.projects.length);
}

/** The head's parts: state, agent, title, and the branch when it is not the main one. `p`: null for Skipper's own screen. */
function headParts(s: SessionInfo, p: Project | null): HTMLElement[] {
  const out = [stateIcon(s), agentIcon(s), h("span", "title", taskTitle(s))];
  const w = place(s)?.worktree;
  if (p && w && !bareWorktree(p) && !w.is_main) out.push(h("span", "where mono", branchName(w)));
  // A pane with an open Skipper task names the task.
  const uid = openUid(sk, s.id);
  if (uid) {
    const t = h("span", "where mono mission-task-uid", uid);
    t.title = `Open task ${uid}`;
    out.push(t);
  }
  return out;
}

/** Markup to compare, less the busy icon's phase, which is new on each call. */
const same = (a: Element, b: Element) => strip(a.outerHTML) === strip(b.outerHTML);
const strip = (html: string) => html.replace(/--(water|skiff)-delay:[^;"]*;?\s*/g, "");

/** The session's terminal theme, as its pane shows it. */
function paint(pre: HTMLElement, s: SessionInfo) {
  const id = themeIdFor(s);
  const t = S.themes.find((x) => x.id === id) ?? S.themes.find((x) => x.id === DEFAULT_THEME);
  if (!t) return;
  pre.style.background = t.background;
  pre.style.color = xtermTheme(t).foreground;
  pre.style.colorScheme = isLight(t.background) ? "light" : "dark";
}

/** Brings a tile up to date in place. The tile stays, so its light does not flicker. */
function fresh(b: HTMLElement, s: SessionInfo, p: Project | null) {
  b.style.setProperty("--pc", p ? accent(p) : "var(--accent)");
  const label = `${taskTitle(s)}, ${s.state}`;
  if (b.getAttribute("aria-label") !== label) b.setAttribute("aria-label", label);
  const head = b.querySelector<HTMLElement>(".mission-head");
  if (head) {
    const next = h("span", "mission-head");
    next.append(...headParts(s, p));
    if (!same(head, next)) head.replaceChildren(...next.childNodes);
  }
  const pre = screens.get(s.id);
  if (pre) paint(pre, s);
}

function tile(s: SessionInfo, p: Project | null): HTMLElement {
  const b = h("button", "mission-tile" + (s.id === lit ? " lit" : ""));
  b.type = "button";
  b.tabIndex = -1;
  b.dataset.session = s.id;
  const pre = screens.get(s.id) ?? h("pre", "mission-screen");
  screens.set(s.id, pre);
  b.append(h("span", "mission-head"), pre);
  fresh(b, s, p);
  return b;
}

/** The search's count: the sessions on the board, or those it shows. The top bar counts the waiting ones. */
/**
 * The app colors of a theme on one element and what is in it. The board
 * takes the app theme, not the selected project's; each project's part of a
 * lane takes its project's theme, as the window does when that project is selected.
 */
function themed(el: HTMLElement, p: Project | null) {
  const t = appThemeOf(p);
  if (!t || el.dataset.theme === t.id) return;
  el.dataset.theme = t.id;
  for (const [k, v] of Object.entries(appThemeVars(t))) el.style.setProperty(k, v);
}

/**
 * A big sideways scrollbar under the lanes, in place of the thin native one.
 * Drag the thumb. A press on the track puts the thumb there and drags on.
 * The wheel over it scrolls sideways. No snap, no pages: the board follows the pointer.
 */
function scrollbar(board: HTMLElement): HTMLElement {
  const track = h("div", "mission-scroll");
  const thumb = h("div", "mission-thumb");
  track.appendChild(thumb);
  const fit = () => {
    const w = board.scrollWidth || 1;
    thumb.style.left = `${(board.scrollLeft / w) * 100}%`;
    thumb.style.width = `${(board.clientWidth / w) * 100}%`;
    track.hidden = board.scrollWidth <= board.clientWidth;
  };
  board.addEventListener("scroll", fit, { passive: true });
  new ResizeObserver(fit).observe(board);
  track.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const scale = board.scrollWidth / track.clientWidth;
    // On the track: the thumb's middle jumps under the pointer first.
    if (e.target !== thumb) {
      const t = thumb.getBoundingClientRect();
      board.scrollLeft += (e.clientX - (t.left + t.width / 2)) * scale;
    }
    const x0 = e.clientX;
    const left0 = board.scrollLeft;
    track.setPointerCapture(e.pointerId);
    track.classList.add("dragging");
    const move = (m: PointerEvent) => (board.scrollLeft = left0 + (m.clientX - x0) * scale);
    const up = () => {
      track.removeEventListener("pointermove", move);
      track.classList.remove("dragging");
    };
    track.addEventListener("pointermove", move);
    track.addEventListener("pointerup", up, { once: true });
    track.addEventListener("pointercancel", up, { once: true });
  });
  track.addEventListener(
    "wheel",
    (e) => {
      board.scrollLeft += e.deltaY || e.deltaX;
      e.preventDefault();
    },
    { passive: false },
  );
  requestAnimationFrame(fit);
  return track;
}

/** The lane head's rollup, put in or taken out in place. */
function roll(head: Element, l: Lane) {
  const old = head.querySelector(".tree-roll");
  const next = rollup(l.projects.map((x) => x.p));
  if (!next) old?.remove();
  else if (!old) head.appendChild(next);
  else if (!same(old, next)) old.replaceWith(next);
}

/**
 * The light when its tile left the board: the next tile in the old order,
 * else the one before it, else the first.
 */
export function relight(was: string[], now: string[], lit: string | null): string | null {
  if (lit && now.includes(lit)) return lit;
  const keep = new Set(now);
  const at = lit ? was.indexOf(lit) : -1;
  if (at >= 0) {
    const next = was.slice(at + 1).find((k) => keep.has(k)) ?? was.slice(0, at).reverse().find((k) => keep.has(k));
    if (next) return next;
  }
  return now[0] ?? null;
}

function build(all = lanes()) {
  const was = order.flat();
  sk = skipperView();
  order = [...(sk ? [skipperKeys(sk)] : []), ...all.map((l) => l.projects.flatMap((x) => x.list.map((s) => s.id)))];
  const ids = new Set(order.flat());
  for (const id of screens.keys()) if (!ids.has(id)) screens.delete(id);
  lit = relight(was, order.flat(), lit);

  // A rebuild on the shown board keeps where each lane and the board are scrolled.
  const old = root.querySelector<HTMLElement>(".mission-lanes");
  const left = old?.scrollLeft ?? 0;
  const tops = new Map<string, number>();
  for (const el of old?.querySelectorAll<HTMLElement>(".mission-lane") ?? []) tops.set(el.getAttribute("aria-label") ?? "", el.scrollTop);

  search.title = `Search sessions: ${keyLabel("palette")}`;
  scope.title = `Only the running sessions, or all: ${keyLabel("focus-mode")}`;

  const board = h("div", "mission-lanes");
  if (sk) board.appendChild(skipperLane(sk, lit, sk.self ? tile(sk.self, null) : null));
  for (const l of all) {
    const lane = h("section", "mission-lane");
    lane.setAttribute("aria-label", l.name);
    const head = h("div", "mission-lane-head");
    head.appendChild(h("span", "mission-lane-name", l.name));
    roll(head, l);
    lane.appendChild(head);
    for (const { p, list } of l.projects) {
      const group = h("div", "mission-group");
      group.dataset.project = p.name;
      themed(group, p);
      const row = h("div", "mission-project");
      row.style.setProperty("--pc", accent(p));
      row.append(projectBadge(p), h("span", "project-name", p.name));
      group.appendChild(row);
      for (const s of list) group.appendChild(tile(s, p));
      lane.appendChild(group);
    }
    board.appendChild(lane);
  }
  if (!all.length && !sk) board.appendChild(h("div", "mission-empty", "No sessions"));
  themed(root, null);
  root.replaceChildren(board, scrollbar(board));
  filter(all);

  if (root.hidden || !old) return;
  board.scrollLeft = left;
  for (const el of board.querySelectorAll<HTMLElement>(".mission-lane")) el.scrollTop = tops.get(el.getAttribute("aria-label") ?? "") ?? 0;
}

/** Everything but the structure, brought up to date in place. */
function update(all: Lane[]) {
  themed(root, null);
  for (const g of root.querySelectorAll<HTMLElement>(".mission-group")) {
    const p = S.projects.find((x) => x.name === g.dataset.project);
    if (p) themed(g, p);
  }
  if (sk) {
    freshSkipper(root, sk);
    const b = sk.self && tileOf(sk.self.id);
    if (b && sk.self) fresh(b, sk.self, null);
  }
  const heads = root.querySelectorAll(".mission-lane:not(.skipper) > .mission-lane-head");
  for (const [i, l] of all.entries()) {
    if (heads[i]) roll(heads[i], l);
    for (const { p, list } of l.projects) {
      for (const s of list) {
        const b = tileOf(s.id);
        if (b) fresh(b, s, p);
      }
    }
  }
  filter(all);
}

/** The board's structure: lanes, their projects, and the sessions in each. A change rebuilds it. */
function signature(all: Lane[], v: SkipperView | null): string {
  return skipperSignature(v) + "|" + all.map((l) => l.name + ":" + l.projects.map((x) => x.p.name + "=" + x.list.map((s) => s.id).join(",")).join(";")).join("|");
}

async function read() {
  if (reading) return;
  reading = true;
  await Promise.all(
    [...screens].map(async ([id, pre]) => {
      try {
        const lines = await invoke<string[]>("session_screen", { session: id, rows: ROWS });
        if (paused) return;
        const text = lines.join("\n");
        if (pre.textContent !== text) pre.textContent = text;
        pre.classList.remove("note");
      } catch (e) {
        if (paused) return;
        // An older skiffd does not know the request.
        pre.textContent = /unknown variant|bad request/.test(String(e)) ? "Update skiffd to see screens." : String(e);
        pre.classList.add("note");
      }
    }),
  );
  reading = false;
}

function tick() {
  if (paused) return;
  const all = lanes();
  const v = skipperView();
  const sig = signature(all, v);
  if (sig !== shape) {
    shape = sig;
    build(all);
  } else {
    sk = v;
    update(all);
  }
  skipper.poll();
  void read();
}
skipper.onChange(() => {
  if (!root.hidden && !paused) tick();
});

/** `scroll`: bring the screen into view, for keys. The pointer is already on it. */
function light(id: string, scroll = true) {
  lit = id;
  for (const t of root.querySelectorAll<HTMLElement>(".mission-tile")) {
    t.classList.toggle("lit", t.dataset.session === id);
    t.toggleAttribute("aria-current", t.dataset.session === id);
  }
  // The search's focus frame takes the lit screen's project color.
  find.style.setProperty("--pc", tileOf(id)?.style.getPropertyValue("--pc") ?? "");
  if (scroll) root.querySelector(".mission-tile.lit")?.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/** Arrows and list keys: Up and Down in a lane, Left and Right to the same place in the next lane. */
function step(key: string): boolean {
  const order = shown();
  const li = order.findIndex((l) => l.includes(lit ?? ""));
  if (li < 0) return false;
  const at = order[li].indexOf(lit!);
  const near = (l: string[]) => l[Math.min(at, l.length - 1)];
  // Left and Right skip a lane the search leaves empty.
  const next = (d: number) => {
    for (let i = li + d; i >= 0 && i < order.length; i += d) if (order[i].length) return near(order[i]);
    return lit!;
  };
  if (key === "ArrowDown") light(order[li][Math.min(at + 1, order[li].length - 1)]);
  else if (key === "ArrowUp") light(order[li][Math.max(at - 1, 0)]);
  else if (key === "ArrowRight") light(next(1));
  else if (key === "ArrowLeft") light(next(-1));
  else return false;
  return true;
}

export function missionOpen(): boolean {
  return !root.hidden;
}

function tileOf(id: string | null): HTMLElement | null {
  return id ? root.querySelector<HTMLElement>(`.mission-tile[data-session="${CSS.escape(id)}"]`) : null;
}

/**
 * What the motion code (missionMotion.ts) drives. The board owns what shows;
 * the motion owns when and how it comes and goes.
 */
export const board = {
  root,
  lit: () => lit,
  /** The session the lit tile stands for: its own, or a Skipper task's pane. */
  litSession: () => (isSkipperKey(lit) ? skipperSession(sk, lit!) : lit),
  tile: tileOf,
  /** Builds the board lit on `focus` and reads every screen once. Shows nothing. */
  async prepare(focus: string | null) {
    paused = false;
    lit = focus;
    await skipper.read();
    const all = lanes();
    shape = signature(all, skipperView());
    build(all);
    await read();
  },
  /** With the root shown: scroll the lit screen into view and keep the screens fresh. */
  start() {
    paused = false;
    // Focus mode and maximize leave the top bar inert. The board shows it, and its search is in it.
    topbar()?.toggleAttribute("inert", false);
    light(lit ?? "");
    window.clearInterval(timer);
    timer = window.setInterval(tick, TICK);
  },
  /** No rebuild, update or read from here on: an animation is about to measure. */
  stop() {
    paused = true;
    window.clearInterval(timer);
  },
  /** Empty and hidden. */
  clear() {
    window.clearInterval(timer);
    topbar()?.toggleAttribute("inert", document.body.classList.contains("focus-mode"));
    root.hidden = true;
    screens.clear();
    skipper.clear();
    sk = null;
    order = [];
    root.replaceChildren();
  },
};

root.addEventListener("click", (e) => {
  const t = (e.target as Element).closest<HTMLElement>(".mission-tile");
  if (t?.dataset.session) enter(t.dataset.session);
});

/** Enter or a click: to the session, or a Skipper tile's own action. */
function enter(key: string) {
  if (isSkipperKey(key)) skipperEnter(key);
  else closeMission(key);
}

/** The menu key, Shift+F10 or the project menu's key: the lit Skipper tile's menu. True when one opened. */
export function missionMenu(): boolean {
  if (!missionOpen() || !isSkipperKey(lit)) return false;
  taskMenu(lit!);
  return true;
}
// Only a pointer that moves lights a screen. A key scrolls the board under a
// still pointer, and the screen that lands under it must not take the light back.
let pointer = "";
root.addEventListener("mousemove", (e) => {
  const at = `${e.clientX},${e.clientY}`;
  if (at === pointer) return;
  pointer = at;
  const t = (e.target as Element).closest<HTMLElement>(".mission-tile");
  if (t?.dataset.session && t.dataset.session !== lit) light(t.dataset.session, false);
});
// A plain wheel scrolls the lanes sideways where it cannot scroll a lane:
// over a lane head, or over a lane that fits. Shift+wheel always does.
root.addEventListener(
  "wheel",
  (e) => {
    const board = root.querySelector<HTMLElement>(".mission-lanes");
    if (!board || e.shiftKey || e.deltaX || board.scrollWidth <= board.clientWidth) return;
    const lane = (e.target as Element).closest<HTMLElement>(".mission-lane");
    const head = (e.target as Element).closest(".mission-lane-head");
    if (lane && !head && lane.scrollHeight > lane.clientHeight) return;
    board.scrollLeft += e.deltaY;
    e.preventDefault();
  },
  { passive: false },
);
/** The list keys step the screens as the arrows do: Ctrl+Shift+Up/Down in a lane, Ctrl+Shift+Left/Right across. */
const LIST_KEYS: Partial<Record<Action, string>> = {
  "session-next": "ArrowDown",
  "session-prev": "ArrowUp",
  "list-back": "ArrowRight",
  "list-project": "ArrowLeft",
};

/** A list key on the open board. The menu bar sends its accelerators here through `runAction()`. */
export function missionKey(a: Action | null): boolean {
  const key = a && LIST_KEYS[a];
  if (!key || !missionOpen()) return false;
  step(key);
  return true;
}

const topbar = () => document.getElementById("topbar");

/** Ctrl+Shift+P here: into the search, or back out to the board. The search stays. */
export function toggleSearch() {
  if (document.activeElement === search) root.focus();
  else {
    search.focus();
    search.select();
  }
}

search.addEventListener("input", () => filter(lanes(), true));
/** The board's keys, on the board and in its search. */
function onKey(e: KeyboardEvent) {
  const a = actionFor(e);
  if (a === "palette") toggleSearch();
  // The first Esc clears the search; the next one closes.
  else if (e.key === "Escape" && search.value) {
    search.value = "";
    filter(lanes());
    if (lit) light(lit);
  } else if (e.key === "Escape" || a === "mission-control") closeMission();
  else if (a === "maximize") maximizeLit();
  else if (a === "focus-mode") toggleRunning();
  // The top bar's actions, as its buttons run them.
  else if (a === "next-waiting") goToSession(nextWaiting());
  else if (a === "settings") {
    closeMission();
    void settings.open();
  } else if (missionKey(a)) {
    // Stepped. The list keys work in the search too.
  } else if (a === "project-menu" || e.key === "ContextMenu" || (e.key === "F10" && e.shiftKey)) {
    if (!missionMenu()) return;
  } else if (e.key === "Enter" && lit) enter(lit);
  // Plain arrows step too. In the search, Left and Right move the caret.
  else if (e.ctrlKey || e.shiftKey || e.altKey || e.metaKey) return;
  else if ((e.target === search && (e.key === "ArrowLeft" || e.key === "ArrowRight")) || !step(e.key)) return;
  e.preventDefault();
  e.stopPropagation();
}
root.addEventListener("keydown", onKey);
find.addEventListener("keydown", onKey);
