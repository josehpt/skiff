/**
 * Mission control (#88): every session's screen over the whole window, one
 * lane per fleet. Read-only text from skiffd; no pane, no PTY resize.
 * Ctrl+Shift+A or Esc closes it. A click or Enter goes to the session.
 * This file is the board. missionMotion.ts opens and closes it.
 */

import { invoke } from "@tauri-apps/api/core";

import type { Project, SessionInfo } from "../platform/types";
import { agentIcon } from "../appearance/agentIcon";
import { stateIcon } from "../appearance/stateIcon";
import { DEFAULT_THEME, isLight } from "../appearance/colors";
import { themeIdFor, xtermTheme } from "../appearance/themes";
import { actionFor } from "../app/keys";
import { S, sessions } from "../app/state";
import { accent, place } from "../app/stateQueries";
import { h } from "../ui/dom";
import { branchName, taskTitle } from "./model";
import { worktreeLines } from "./sidebarLines";
import { bareWorktree, projectBadge, rollup, treeSections } from "./tree";
import { closeMission, maximizeLit } from "./missionMotion";

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
let shape = "";
let timer: number | undefined;
let reading = false;
// Set by board.stop(): no rebuild, update or read until board.start().
let paused = false;
const screens = new Map<string, HTMLElement>();

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

/** Each fleet is a lane, as in the tree; "Projects" only when it has sessions. Projects with no sessions are left out. */
function lanes(): Lane[] {
  return treeSections()
    .map((sec) => ({
      name: sec.fleet ?? "Projects",
      projects: sec.projects.map((p) => ({ p, list: projectSessions(p) })).filter((x) => x.list.length),
    }))
    .filter((l, i) => l.projects.length || i > 0);
}

/** The head's parts: state, agent, title, and the branch when it is not the main one. */
function headParts(s: SessionInfo, p: Project): HTMLElement[] {
  const out = [stateIcon(s), agentIcon(s), h("span", "title", taskTitle(s))];
  const w = place(s)?.worktree;
  if (w && !bareWorktree(p) && !w.is_main) out.push(h("span", "where mono", branchName(w)));
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
function fresh(b: HTMLElement, s: SessionInfo, p: Project) {
  b.style.setProperty("--pc", accent(p));
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

function tile(s: SessionInfo, p: Project): HTMLElement {
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

/** The bar's counts: all sessions on the board, and those that wait. */
function counts(bar: HTMLElement, ids: Set<string>) {
  const n = bar.querySelector(".mission-count");
  if (n) n.textContent = `${ids.size} sessions`;
  const waiting = [...sessions.values()].filter((s) => s.state === "waiting").length;
  let w = bar.querySelector(".mission-waiting");
  if (!waiting) w?.remove();
  else {
    if (!w) bar.appendChild((w = h("span", "mission-waiting")));
    w.textContent = `${waiting} waiting`;
  }
}

/** The lane head's rollup, put in or taken out in place. */
function roll(head: Element, l: Lane) {
  const old = head.querySelector(".tree-roll");
  const next = rollup(l.projects.map((x) => x.p));
  if (!next) old?.remove();
  else if (!old) head.appendChild(next);
  else if (!same(old, next)) old.replaceWith(next);
}

function build(all = lanes()) {
  order = all.map((l) => l.projects.flatMap((x) => x.list.map((s) => s.id)));
  const ids = new Set(order.flat());
  for (const id of screens.keys()) if (!ids.has(id)) screens.delete(id);
  if (!lit || !ids.has(lit)) lit = order.flat()[0] ?? null;

  // A rebuild on the shown board keeps where each lane and the board are scrolled.
  const old = root.querySelector<HTMLElement>(".mission-lanes");
  const left = old?.scrollLeft ?? 0;
  const tops = new Map<string, number>();
  for (const el of old?.querySelectorAll<HTMLElement>(".mission-lane") ?? []) tops.set(el.getAttribute("aria-label") ?? "", el.scrollTop);

  const bar = h("div", "mission-bar");
  bar.append(h("span", "mission-name", "Mission control"), h("span", "mission-count"));
  counts(bar, ids);

  const board = h("div", "mission-lanes");
  for (const l of all) {
    const lane = h("section", "mission-lane");
    lane.setAttribute("aria-label", l.name);
    const head = h("div", "mission-lane-head");
    head.appendChild(h("span", "mission-lane-name", l.name));
    roll(head, l);
    lane.appendChild(head);
    if (!l.projects.length) lane.appendChild(h("div", "mission-none", "No sessions"));
    for (const { p, list } of l.projects) {
      const row = h("div", "mission-project");
      row.style.setProperty("--pc", accent(p));
      row.append(projectBadge(p), h("span", "project-name", p.name));
      lane.appendChild(row);
      for (const s of list) lane.appendChild(tile(s, p));
    }
    board.appendChild(lane);
  }
  if (!all.length) board.appendChild(h("div", "mission-empty", "No sessions"));
  root.replaceChildren(bar, board);

  if (root.hidden || !old) return;
  board.scrollLeft = left;
  for (const el of board.querySelectorAll<HTMLElement>(".mission-lane")) el.scrollTop = tops.get(el.getAttribute("aria-label") ?? "") ?? 0;
}

/** Everything but the structure, brought up to date in place. */
function update(all: Lane[]) {
  const bar = root.querySelector<HTMLElement>(".mission-bar");
  if (bar) counts(bar, new Set(order.flat()));
  const heads = root.querySelectorAll(".mission-lane-head");
  for (const [i, l] of all.entries()) {
    if (heads[i]) roll(heads[i], l);
    for (const { p, list } of l.projects) {
      for (const s of list) {
        const b = tileOf(s.id);
        if (b) fresh(b, s, p);
      }
    }
  }
}

/** The board's structure: lanes, their projects, and the sessions in each. A change rebuilds it. */
function signature(all: Lane[]): string {
  return all.map((l) => l.name + ":" + l.projects.map((x) => x.p.name + "=" + x.list.map((s) => s.id).join(",")).join(";")).join("|");
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
        pre.textContent = /unknown variant|bad request/.test(String(e)) ? "Restart skiffd to see screens." : String(e);
        pre.classList.add("note");
      }
    }),
  );
  reading = false;
}

function tick() {
  if (paused) return;
  const all = lanes();
  const sig = signature(all);
  if (sig !== shape) {
    shape = sig;
    build(all);
  } else update(all);
  void read();
}

/** `scroll`: bring the screen into view, for keys. The pointer is already on it. */
function light(id: string, scroll = true) {
  lit = id;
  for (const t of root.querySelectorAll<HTMLElement>(".mission-tile")) t.classList.toggle("lit", t.dataset.session === id);
  if (scroll) root.querySelector(".mission-tile.lit")?.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/** Arrows: Up and Down in a lane, Left and Right to the same place in the next lane. */
function step(key: string): boolean {
  const li = order.findIndex((l) => l.includes(lit ?? ""));
  if (li < 0) return false;
  const at = order[li].indexOf(lit!);
  const near = (l: string[]) => l[Math.min(at, l.length - 1)];
  // Left and Right skip a lane with no sessions.
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
  tile: tileOf,
  /** Builds the board lit on `focus` and reads every screen once. Shows nothing. */
  async prepare(focus: string | null) {
    paused = false;
    lit = focus;
    const all = lanes();
    shape = signature(all);
    build(all);
    await read();
  },
  /** With the root shown: scroll the lit screen into view and keep the screens fresh. */
  start() {
    paused = false;
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
    root.hidden = true;
    screens.clear();
    root.replaceChildren();
  },
};

root.addEventListener("click", (e) => {
  const t = (e.target as Element).closest<HTMLElement>(".mission-tile");
  if (t?.dataset.session) closeMission(t.dataset.session);
});
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
    const head = (e.target as Element).closest(".mission-lane-head, .mission-bar");
    if (lane && !head && lane.scrollHeight > lane.clientHeight) return;
    board.scrollLeft += e.deltaY;
    e.preventDefault();
  },
  { passive: false },
);
root.addEventListener("keydown", (e) => {
  const a = actionFor(e);
  if (e.key === "Escape" || a === "mission-control") closeMission();
  else if (a === "maximize") maximizeLit();
  else if (e.key === "Enter" && lit) closeMission(lit);
  else if (e.ctrlKey || e.altKey || e.metaKey || !step(e.key)) return;
  e.preventDefault();
  e.stopPropagation();
});
