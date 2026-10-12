import { invoke } from "@tauri-apps/api/core";

import { isUnread } from "./model";
import type { Project } from "../platform/types";

import { $, button, h } from "../ui/dom";

import { projectIcon } from "../appearance/projectIcon";
import { showError } from "../ui/alerts";
import { fleetMenu, projectMenu } from "./menus";

import { addProject } from "../app/panels";

import { render } from "../app/renderRequest";

import { OTHER, S, sessions } from "../app/state";
import { accent, place } from "../app/stateQueries";

import { selectProject } from "./view";
import { worktreeLines } from "./sidebarLines";

/** A circle around the chip's icon. Working: 12 dashes, and a turn of one dash loops without a seam. */
function railRing(state: "working" | "waiting" | "unread"): Element {
  const t = document.createElement("template");
  t.innerHTML = `<svg class="rail-ring ${state}" viewBox="0 0 36 36" aria-hidden="true"><circle cx="18" cy="18" r="16" pathLength="24"></circle></svg>`;
  return t.content.firstChild as Element;
}

export interface Fleet {
  name: string;
  projects: Project[];
}

/** A row on the rail: a project chip, or a fleet tile that stands for its projects. */
export type RailRow = { kind: "project"; p: Project; inFleet: boolean } | { kind: "fleet"; f: Fleet; open: boolean };

/** The project last selected in each fleet. A click on the tile goes back to it. */
const lastIn = new Map<string, string>();
/** The selected project's fleet, folded by a click on its tile. It opens again when another project is selected. */
let folded: string | null = null;

const selectedFleet = () => S.projects.find((p) => p.name === S.selectedProject)?.fleet ?? null;

/** Fleets and lone projects in rail order. A fleet stands where its first project is. */
function railEntries(): (Project | Fleet)[] {
  const out: (Project | Fleet)[] = [];
  const fleets = new Map<string, Fleet>();
  for (const p of S.projects) {
    if (!p.fleet) {
      out.push(p);
      continue;
    }
    let f = fleets.get(p.fleet);
    if (!f) {
      f = { name: p.fleet, projects: [] };
      fleets.set(p.fleet, f);
      out.push(f);
    }
    f.projects.push(p);
  }
  return out;
}

const isFleet = (e: Project | Fleet): e is Fleet => "projects" in e;

/** Every project in rail order, the ones in folded fleets too. The cycle keys step through it. */
export function railProjects(): Project[] {
  return railEntries().flatMap((e) => (isFleet(e) ? e.projects : [e]));
}

/** What the rail shows: the selected project's fleet opens, the others fold to one tile. */
export function railRows(): RailRow[] {
  const sel = selectedFleet();
  if (folded !== sel) folded = null;
  return railEntries().flatMap((e): RailRow[] => {
    if (!isFleet(e)) return [{ kind: "project", p: e, inFleet: false }];
    const open = e.name === sel && folded !== e.name;
    return [{ kind: "fleet", f: e, open }, ...(open ? e.projects.map((p) => ({ kind: "project" as const, p, inFleet: true })) : [])];
  });
}

/** A click or Ctrl+Shift+digit on a fleet tile: the fleet's last project, which opens it. On the open fleet, the tile folds it. */
export function pickFleet(f: Fleet) {
  if (f.name === selectedFleet()) {
    folded = folded === f.name ? null : f.name;
    render();
    return;
  }
  const last = f.projects.find((p) => p.name === lastIn.get(f.name)) ?? f.projects[0];
  if (last) selectProject(last.name);
}

/** Ctrl+Shift+1..9: the visible rail row with that number. */
export function pickRow(n: number) {
  const r = railRows()[n - 1];
  if (!r) return;
  if (r.kind === "project") selectProject(r.p.name);
  else pickFleet(r.f);
}

/** The digit for row `i`, as Ctrl+Shift+digit reads it. */
const digitHint = (name: string, i: number) => (i < 9 ? `${name} (${navigator.userAgent.includes("Macintosh") ? "⌘" : "Ctrl"}+Shift+${i + 1})` : name);

export function renderRail() {
  const rail = $<HTMLElement>("rail");
  const hasOther = worktreeLines(null).length > 0;
  // The last outside session ended: Other goes away, and the first project shows.
  if (!hasOther && S.selectedProject === OTHER) S.selectedProject = railProjects()[0]?.name ?? null;
  const sel = S.projects.find((p) => p.name === S.selectedProject);
  if (sel?.fleet) lastIn.set(sel.fleet, sel.name);
  const waitingIn = (ps: Project[]) => [...sessions.values()].filter((s) => s.state === "waiting" && ps.includes(place(s)?.project as Project)).length;
  // Renders run while agents print. A rebuilt chip loses its hover and hides
  // its icon until trimmed again, so the rail changes only when what it shows does.
  const unreadIn = (ps: Project[]) => [...sessions.values()].filter((s) => isUnread(s) && ps.includes(place(s)?.project as Project)).length;
  const workingIn = (ps: Project[]) => [...sessions.values()].some((s) => s.state === "working" && ps.includes(place(s)?.project as Project));
  const rows = railRows();
  const sig = JSON.stringify([
    S.selectedProject,
    hasOther,
    rows.map((r) =>
      r.kind === "project"
        ? [r.p.name, r.p.short, r.p.icon, accent(r.p), r.inFleet, waitingIn([r.p]), unreadIn([r.p]), workingIn([r.p])]
        : [r.f.name, r.open, r.f.projects.map((p) => accent(p)), waitingIn(r.f.projects), unreadIn(r.f.projects), workingIn(r.f.projects)],
    ),
  ]);
  if (rail.dataset.sig === sig) return;
  rail.dataset.sig = sig;
  rail.replaceChildren();
  // One ring tells the state. It turns, dashed and blue, while an agent works.
  // It stops as a solid circle: amber when an agent waits, green when output is unread.
  // Waiting wins, then working, then unread.
  const ringFor = (ps: Project[]) => (waitingIn(ps) ? "waiting" : workingIn(ps) ? "working" : unreadIn(ps) ? "unread" : null);
  let block: HTMLElement | null = null;
  rows.forEach((r, i) => {
    if (r.kind === "fleet") {
      block = null;
      const item = h("div", "rail-item");
      const current = !r.open && !!sel && r.f.projects.includes(sel);
      const b = button("rail-chip rail-fleet" + (current ? " active" : ""), "", () => pickFleet(r.f));
      const tile = h("span", "fleet-tile");
      for (const p of r.f.projects.slice(0, 4)) {
        const sq = h("span");
        sq.style.background = accent(p);
        tile.appendChild(sq);
      }
      b.appendChild(tile);
      b.dataset.fleet = r.f.name;
      if (current && sel) b.style.setProperty("--pc", accent(sel));
      b.title = digitHint(r.f.name, i);
      b.setAttribute("aria-expanded", String(r.open));
      const waiting = waitingIn(r.f.projects);
      const unread = unreadIn(r.f.projects);
      b.setAttribute("aria-label", `${r.f.name}, ${r.f.projects.length} projects` + (waiting ? `, ${waiting} waiting` : unread ? `, ${unread} unread` : ""));
      if (current) b.setAttribute("aria-current", "true");
      b.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        fleetMenu(r.f, e.clientX, e.clientY);
      });
      item.appendChild(b);
      // An open fleet's projects show theirs.
      const ring = r.open ? null : ringFor(r.f.projects);
      if (ring) item.appendChild(railRing(ring));
      if (!r.open && (waiting || unread)) b.classList.add("attention");
      if (r.open) {
        block = h("div", "rail-fleet-open");
        block.appendChild(item);
        rail.appendChild(block);
      } else {
        rail.appendChild(item);
      }
      return;
    }
    const p = r.p;
    if (!r.inFleet) block = null;
    const waiting = waitingIn([p]);
    const unread = unreadIn([p]);
    const item = h("div", "rail-item");
    const b = button("rail-chip" + (p.name === S.selectedProject ? " active" : ""), p.icon ? "" : p.short, () => selectProject(p.name));
    if (p.icon) {
      b.classList.add("has-icon");
      b.appendChild(projectIcon(p, 24));
    }
    b.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      projectMenu(p, e.clientX, e.clientY);
    });
    b.addEventListener("mousedown", (e) => dragProject(e, item, p));
    b.dataset.project = p.name;
    item.dataset.project = p.name;
    b.style.setProperty("--pc", accent(p));
    b.title = digitHint(p.name, i);
    b.setAttribute("aria-label", p.name + (waiting ? `, ${waiting} waiting` : unread ? `, ${unread} unread` : ""));
    if (p.name === S.selectedProject) b.setAttribute("aria-current", "true");
    item.appendChild(b);
    const ring = ringFor([p]);
    if (ring) item.appendChild(railRing(ring));
    if (waiting || unread) b.classList.add("attention");
    ((r.inFleet && block) || rail).appendChild(item);
  });
  if (hasOther) {
    const item = h("div", "rail-item");
    const on = S.selectedProject === OTHER;
    const b = button("rail-chip rail-other" + (on ? " active" : ""), "…", () => {
      S.selectedProject = OTHER;
      render();
    });
    b.title = "Other: sessions outside every project";
    b.setAttribute("aria-label", "Other sessions");
    if (on) b.setAttribute("aria-current", "true");
    item.appendChild(b);
    rail.appendChild(item);
  }
  const add = button("rail-chip rail-add", "+", () => void addProject.open());
  add.title = "Add project";
  add.setAttribute("aria-label", "Add project");
  rail.appendChild(add);
}

/**
 * Drag a rail chip to reorder projects. A press that does not move stays a
 * click. The new order is the rail order, so a fleet keeps its projects
 * together; a project drops out of a fleet only through its Fleet… item.
 */
function dragProject(down: MouseEvent, item: HTMLElement, dragged: Project) {
  if (down.button !== 0 || S.projects.length < 2) return;
  const sy = down.clientY;
  const items = [...$<HTMLElement>("rail").querySelectorAll<HTMLElement>(".rail-item[data-project]")];
  const from = items.indexOf(item);
  let to = from;
  let active = false;
  const mark = () => {
    items.forEach((el, i) => {
      el.classList.toggle("drop-before", active && i === to && to < from);
      el.classList.toggle("drop-after", active && i === to && to > from);
    });
  };
  const move = (m: MouseEvent) => {
    if (!active) {
      if (Math.abs(m.clientY - sy) < 5) return;
      active = true;
      item.classList.add("dragging");
      document.body.classList.add("dragging-rail");
    }
    to = items.findIndex((el) => m.clientY < el.getBoundingClientRect().bottom);
    if (to < 0) to = items.length - 1;
    mark();
  };
  const up = () => {
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", up);
    if (!active) return;
    // The click that follows the release would select the project.
    // A release off the chip fires no click, so the guard must not outlive this event.
    const swallow = (c: Event) => c.stopPropagation();
    window.addEventListener("click", swallow, true);
    setTimeout(() => window.removeEventListener("click", swallow, true), 0);
    document.body.classList.remove("dragging-rail");
    item.classList.remove("dragging");
    active = false;
    mark();
    if (to === from) return;
    const order = railProjects();
    const target = order.find((p) => p.name === items[to].dataset.project);
    order.splice(order.indexOf(dragged), 1);
    if (target) order.splice(order.indexOf(target) + (to > from ? 1 : 0), 0, dragged);
    S.projects = order;
    render();
    invoke("reorder_projects", { order: S.projects.map((x) => x.name) }).catch(showError);
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up);
}
