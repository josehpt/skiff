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
import { actionFor, keyLabel } from "../app/keys";
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

/** The tree's sections with sessions, each a lane. Projects with no sessions are left out. */
function lanes(): Lane[] {
  return treeSections()
    .map((sec) => ({
      name: sec.fleet ?? "Projects",
      projects: sec.projects.map((p) => ({ p, list: projectSessions(p) })).filter((x) => x.list.length),
    }))
    .filter((l) => l.projects.length);
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
    h("span", "mission-hint", `Click a session to go · Arrows and Enter · Esc or ${keyLabel("mission-control")} closes`),
  );
  const waiting = count((s) => s.state === "waiting");
  if (waiting) bar.insertBefore(h("span", "mission-waiting", `${waiting} waiting`), bar.lastChild);

  const board = h("div", "mission-lanes");
  for (const l of all) {
    const lane = h("section", "mission-lane");
    lane.setAttribute("aria-label", l.name);
    const head = h("div", "mission-lane-head");
    head.appendChild(h("span", "mission-lane-name", l.name));
    const roll = rollup(l.projects.map((x) => x.p));
    if (roll) head.appendChild(roll);
    lane.appendChild(head);
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

function light(id: string) {
  lit = id;
  for (const t of root.querySelectorAll<HTMLElement>(".mission-tile")) t.classList.toggle("lit", t.dataset.session === id);
  root.querySelector(".mission-tile.lit")?.scrollIntoView({ block: "nearest", inline: "nearest" });
}

/** Arrows: Up and Down in a lane, Left and Right to the same place in the next lane. */
function step(key: string): boolean {
  const li = order.findIndex((l) => l.includes(lit ?? ""));
  if (li < 0) return false;
  const at = order[li].indexOf(lit!);
  const near = (l: string[]) => l[Math.min(at, l.length - 1)];
  if (key === "ArrowDown") light(order[li][Math.min(at + 1, order[li].length - 1)]);
  else if (key === "ArrowUp") light(order[li][Math.max(at - 1, 0)]);
  else if (key === "ArrowRight") light(near(order[Math.min(li + 1, order.length - 1)]));
  else if (key === "ArrowLeft") light(near(order[Math.max(li - 1, 0)]));
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
  root.focus();
  light(lit ?? "");
  void read();
  timer = window.setInterval(tick, TICK);
}

/** `go`: the session to show. Without it, the keys go back where they were. */
export function closeMission(go?: string) {
  if (!missionOpen()) return;
  root.hidden = true;
  window.clearInterval(timer);
  screens.clear();
  root.replaceChildren();
  if (go) return revealSession(go);
  if (returnTo?.isConnected) returnTo.focus();
  else refocusTerminal();
}

export function toggleMission() {
  if (missionOpen()) closeMission();
  else openMission();
}

root.addEventListener("click", (e) => {
  const t = (e.target as Element).closest<HTMLElement>(".mission-tile");
  if (t?.dataset.session) closeMission(t.dataset.session);
});
root.addEventListener("mouseover", (e) => {
  const t = (e.target as Element).closest<HTMLElement>(".mission-tile");
  if (t?.dataset.session && t.dataset.session !== lit) light(t.dataset.session);
});
root.addEventListener("keydown", (e) => {
  if (e.key === "Escape" || actionFor(e) === "mission-control") closeMission();
  else if (e.key === "Enter" && lit) closeMission(lit);
  else if (e.ctrlKey || e.altKey || e.metaKey || !step(e.key)) return;
  e.preventDefault();
  e.stopPropagation();
});
