/** The side panel's tree: fleets as section heads, projects as rows, each folded or open (#87). */

import type { Project, SessionInfo } from "../platform/types";

import { isUnread } from "./model";
import { BELL, SKIFF } from "../appearance/stateIcon";
import { projectIcon } from "../appearance/projectIcon";
import { storedSet } from "../app/stored";
import { S, sessions } from "../app/state";
import { place } from "../app/stateQueries";
import { h } from "../ui/dom";
import { icon } from "../ui/icons";
import { animateFold } from "../ui/fold";

/** Folded rows: `fleet:<name>` and `project:<name>`. Every row starts open. */
export const folded = storedSet("skiff.tree.folded");

export const fleetKey = (name: string) => `fleet:${name}`;
export const projectKey = (name: string) => `project:${name}`;

export interface Section {
  /** null: the projects in no fleet. */
  fleet: string | null;
  projects: Project[];
}

/** Projects in no fleet first, then each fleet where its first project stands. */
export function treeSections(): Section[] {
  const loose: Project[] = [];
  const fleets = new Map<string, Project[]>();
  for (const p of S.projects) {
    if (!p.fleet) loose.push(p);
    else if (fleets.has(p.fleet)) fleets.get(p.fleet)!.push(p);
    else fleets.set(p.fleet, [p]);
  }
  return [{ fleet: null, projects: loose }, ...[...fleets].map(([fleet, projects]) => ({ fleet, projects }))];
}

/** The project rows the tree shows, in order. A folded fleet hides its projects. */
export function visibleProjects(): Project[] {
  return treeSections().flatMap((s) => (s.fleet && folded.has(fleetKey(s.fleet)) ? [] : s.projects));
}

export const isOpen = (key: string) => !folded.has(key);

export function setOpen(key: string, open: boolean) {
  if (open) folded.delete(key);
  else folded.add(key);
}

/** The session on screen moved into a folded project or fleet: open them, so its row shows. */
let shownFor: string | null = null;
export function openToFocused() {
  if (S.focused === shownFor) return;
  shownFor = S.focused;
  const s = S.focused ? sessions.get(S.focused) : undefined;
  const p = s ? place(s)?.project : null;
  if (!p) return;
  folded.delete(projectKey(p.name));
  if (p.fleet) folded.delete(fleetKey(p.fleet));
}

/** A folded row's marks: how many sessions under it wait, work, or have unread output. */
export function rollup(projects: Project[]): HTMLElement | null {
  const under = (s: SessionInfo) => projects.includes(place(s)?.project as Project);
  let waiting = 0;
  let working = 0;
  let unread = 0;
  for (const s of sessions.values()) {
    if (!under(s)) continue;
    if (s.state === "waiting") waiting++;
    else if (s.state === "working") working++;
    else if (isUnread(s)) unread++;
  }
  if (!waiting && !working && !unread) return null;
  const el = h("span", "tree-roll");
  const part = (cls: string, paths: string, n: number, label: string) => {
    if (!n) return;
    const p = h("span", cls);
    p.title = `${n} ${label}`;
    p.append(icon(paths), String(n));
    el.appendChild(p);
  };
  part("roll-waiting", BELL, waiting, "waiting");
  part("roll-working", SKIFF, working, "working");
  part("roll-unread", '<circle cx="12" cy="12" r="5" fill="currentColor"></circle>', unread, "unread");
  return el;
}

/** The project's icon, or its letters when it has none (Icon › Letters only). */
export function projectBadge(p: Project): HTMLElement {
  const box = h("span", "tree-icon");
  if (p.icon) box.appendChild(projectIcon(p, 16));
  else box.textContent = p.short;
  return box;
}

/** Open or fold a fleet or project row: `want`, or the other way. */
export function toggleRow(key: string, render: () => void, want = !isOpen(key)) {
  if (want === isOpen(key)) return;
  animateFold(key, want, () => {
    setOpen(key, want);
    render();
  });
}

/** True while a project or fleet is in the air. The tree does not rebuild under it. */
export const treeDrag = { active: false };
