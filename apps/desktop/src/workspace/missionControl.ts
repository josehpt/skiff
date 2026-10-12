/**
 * Mission control (#88): every session's screen over the whole window, one
 * lane per fleet. Read-only text from skiffd; no pane, no PTY resize.
 * Ctrl+Shift+A or Esc closes it. A click or Enter goes to the session.
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
import { refocusTerminal, revealSession } from "./view";

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
let returnTo: HTMLElement | null = null;
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

function tile(s: SessionInfo, p: Project): HTMLElement {
  const b = h("button", "mission-tile" + (s.id === lit ? " lit" : ""));
  b.type = "button";
  b.tabIndex = -1;
  b.dataset.session = s.id;
  b.style.setProperty("--pc", accent(p));
  b.setAttribute("aria-label", `${taskTitle(s)}, ${s.state}`);
  const head = h("span", "mission-head");
  head.append(stateIcon(s), agentIcon(s), h("span", "title", taskTitle(s)));
  const w = place(s)?.worktree;
  if (w && !bareWorktree(p) && !w.is_main) head.appendChild(h("span", "where mono", branchName(w)));
  const pre = screens.get(s.id) ?? h("pre", "mission-screen");
  screens.set(s.id, pre);
  // The session's terminal theme, as its pane shows it.
  const id = themeIdFor(s);
  const t = S.themes.find((x) => x.id === id) ?? S.themes.find((x) => x.id === DEFAULT_THEME);
  if (t) {
    pre.style.background = t.background;
    pre.style.color = xtermTheme(t).foreground;
    pre.style.colorScheme = isLight(t.background) ? "light" : "dark";
  }
  b.append(head, pre);
  return b;
}

function build() {
  const all = lanes();
  order = all.map((l) => l.projects.flatMap((x) => x.list.map((s) => s.id)));
  const ids = new Set(order.flat());
  for (const id of screens.keys()) if (!ids.has(id)) screens.delete(id);
  if (!lit || !ids.has(lit)) lit = order.flat()[0] ?? null;

  const bar = h("div", "mission-bar");
  const count = (f: (s: SessionInfo) => boolean) => [...sessions.values()].filter(f).length;
  bar.append(
    h("span", "mission-name", "Mission control"),
    h("span", "", `${ids.size} sessions`),
  );
  const waiting = count((s) => s.state === "waiting");
  if (waiting) bar.appendChild(h("span", "mission-waiting", `${waiting} waiting`));

  const board = h("div", "mission-lanes");
  for (const [i, l] of all.entries()) {
    const lane = h("section", "mission-lane");
    lane.style.setProperty("--i", String(i));
    lane.setAttribute("aria-label", l.name);
    const head = h("div", "mission-lane-head");
    head.appendChild(h("span", "mission-lane-name", l.name));
    const roll = rollup(l.projects.map((x) => x.p));
    if (roll) head.appendChild(roll);
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
}

/** What the board shows besides the text. A change rebuilds it. */
function signature(): string {
  return lanes()
    .map((l) => l.name + ":" + l.projects.map((x) => x.p.name + "=" + x.list.map((s) => `${s.id}/${s.state}/${s.unread}/${taskTitle(s)}`).join(",")).join(";"))
    .join("|");
}

async function read() {
  if (reading) return;
  reading = true;
  await Promise.all(
    [...screens].map(async ([id, pre]) => {
      try {
        const lines = await invoke<string[]>("session_screen", { session: id, rows: ROWS });
        const text = lines.join("\n");
        if (pre.textContent !== text) pre.textContent = text;
        pre.classList.remove("note");
      } catch (e) {
        // An older skiffd does not know the request.
        pre.textContent = /unknown variant|bad request/.test(String(e)) ? "Restart skiffd to see screens." : String(e);
        pre.classList.add("note");
      }
    }),
  );
  reading = false;
}

function tick() {
  const sig = signature();
  if (sig !== shape) {
    shape = sig;
    build();
  }
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

export function openMission() {
  if (missionOpen()) return;
  returnTo = document.activeElement as HTMLElement | null;
  lit = S.focused;
  shape = signature();
  build();
  root.hidden = false;
  root.classList.remove("opening");
  void root.offsetWidth;
  root.classList.add("opening");
  // A lane rebuilt later does not rise again.
  window.setTimeout(() => root.classList.remove("opening"), 600);
  root.focus();
  light(lit ?? "");
  void read();
  timer = window.setInterval(tick, TICK);
}

/** `go`: the session to show. Without it, the keys go back where they were. */
let leaving = false;

const still = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

function finish() {
  leaving = false;
  root.hidden = true;
  root.classList.remove("leaving", "opening");
  window.clearInterval(timer);
  screens.clear();
  root.replaceChildren();
}

/** Where a session's pane is once it shows: its cell, else the terminal area. */
function paneRect(id: string): DOMRect {
  const el = document.querySelector(`.cell[data-session="${CSS.escape(id)}"]`) ?? document.getElementById("terminal-host");
  return el?.getBoundingClientRect() ?? new DOMRect(0, 0, innerWidth, innerHeight);
}

/**
 * The picked screen flies to where its pane opens, and the board fades
 * around it. The session shows under the board from the start, so the
 * screen lands on it.
 */
function flyTo(id: string) {
  const tile = root.querySelector<HTMLElement>(`.mission-tile[data-session="${CSS.escape(id)}"]`);
  revealSession(id);
  if (!tile || still()) return finish();
  const from = tile.getBoundingClientRect();
  const fly = tile.cloneNode(true) as HTMLElement;
  fly.classList.add("mission-fly", "lit");
  Object.assign(fly.style, { left: `${from.left}px`, top: `${from.top}px`, width: `${from.width}px`, height: `${from.height}px` });
  root.appendChild(fly);
  root.classList.add("leaving");
  // Two frames: the pane is laid out before it is measured.
  requestAnimationFrame(() =>
    requestAnimationFrame(() => {
      const to = paneRect(id);
      const move = `translate(${to.left - from.left}px, ${to.top - from.top}px) scale(${to.width / from.width}, ${to.height / from.height})`;
      const a = fly.animate(
        [{ transform: "none", opacity: 1 }, { transform: move, opacity: 1, offset: 0.8 }, { transform: move, opacity: 0 }],
        { duration: 260, easing: "cubic-bezier(0.2, 0.8, 0.2, 1)", fill: "forwards" },
      );
      a.onfinish = finish;
    }),
  );
}

export function closeMission(go?: string) {
  if (!missionOpen() || leaving) return;
  leaving = true;
  // No rebuild under the animation.
  window.clearInterval(timer);
  if (go) return flyTo(go);
  if (returnTo?.isConnected) returnTo.focus();
  else refocusTerminal();
  if (still()) return finish();
  root.animate([{ opacity: 1 }, { opacity: 0, transform: "scale(1.015)" }], { duration: 140, easing: "ease-in" }).onfinish = finish;
}

export function toggleMission() {
  if (missionOpen()) closeMission();
  else openMission();
}

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
  if (e.key === "Escape" || actionFor(e) === "mission-control") closeMission();
  else if (e.key === "Enter" && lit) closeMission(lit);
  else if (e.ctrlKey || e.altKey || e.metaKey || !step(e.key)) return;
  e.preventDefault();
  e.stopPropagation();
});
