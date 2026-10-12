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

/** The letters on a fleet tile: the initials of a name of several words, else its first three letters. */
export function fleetLetters(name: string): string {
  const words = name.split(/[\s_.-]+/).filter(Boolean);
  const s = words.length > 1 ? words.map((w) => w[0]).join("") : name;
  return [...s.replace(/[^\p{L}\p{N}]/gu, "")].slice(0, 3).join("").toUpperCase();
}

/** The most color bars under the letters. Past that, the bars get too thin to tell apart. */
const STRIP_MAX = 6;

function stripPart(color: string): HTMLElement {
  const s = h("span");
  s.style.background = color;
  return s;
}

/** A fleet tile: its letters in capitals, so it reads apart from a project chip, over one color bar per project. */
function fleetTile(name: string, colors: string[]): HTMLElement {
  const tile = h("span", "fleet-tile");
  const strip = h("span", "fleet-strip");
  strip.append(...colors.slice(0, STRIP_MAX).map(stripPart));
  tile.append(h("span", "fleet-letters", fleetLetters(name)), strip);
  return tile;
}

/** The digit for row `i`, as Ctrl+Shift+digit reads it. */
const digitHint = (name: string, i: number) => (i < 9 ? `${name} (${navigator.userAgent.includes("Macintosh") ? "⌘" : "Ctrl"}+Shift+${i + 1})` : name);

export function renderRail() {
  if (dragging) return;
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
  const openNow = rows.flatMap((r) => (r.kind === "fleet" && r.open ? [r.f.name] : []))[0] ?? null;
  const oldBlock = rail.querySelector<HTMLElement>(".rail-fleet-open:not(.rail-fleet-closing)");
  const closing = openNow !== shownOpen && shownOpen !== undefined && oldBlock ? foldOut(oldBlock) : null;
  const opening = openNow !== shownOpen && shownOpen !== undefined;
  const wasOpen = shownOpen;
  shownOpen = openNow;
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
      b.appendChild(fleetTile(r.f.name, r.f.projects.map(accent)));
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
      b.addEventListener("mousedown", (e) => dragRail(e, item, r.f.projects, r.f));
      item.dataset.fleet = r.f.name;
      item.appendChild(b);
      // An open fleet's projects show theirs.
      const ring = r.open ? null : ringFor(r.f.projects);
      if (ring) item.appendChild(railRing(ring));
      if (!r.open && (waiting || unread)) b.classList.add("attention");
      if (r.open) {
        block = h("div", "rail-fleet-open" + (opening ? " opening" : ""));
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
    b.addEventListener("mousedown", (e) => dragRail(e, item, [p], null));
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
  // The fleet that folded keeps its old chips for one animation under its tile.
  const tile = closing && wasOpen ? rail.querySelector<HTMLElement>(`.rail-item[data-fleet="${CSS.escape(wasOpen)}"]`) : null;
  if (closing && tile?.parentElement === rail) tile.after(closing);
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

/** What a drop does: where the moved projects land, and the fleet each project ends in. */
export interface Drop {
  /** The project the moved ones land next to. */
  anchor: Project;
  side: "before" | "after";
  /** The moved projects' fleet after the drop. Undefined: a fleet moves whole and keeps its name. */
  fleet?: string | null;
  /** A drop on a lone chip: the chip goes into the new fleet too. */
  make?: Project;
  /** The item that shows the drop, and how. */
  el: HTMLElement;
  mark: "drop-before" | "drop-after" | "drop-into";
  label: string;
}

/** The first "Fleet N" no fleet uses. */
export function freeFleetName(): string {
  const used = new Set(S.projects.map((p) => p.fleet));
  let n = 1;
  while (used.has(`Fleet ${n}`)) n++;
  return `Fleet ${n}`;
}

/** The fleets and the rail order after the drop, saved in one write. A fleet left with one project goes away. */
export function applyDrop(moving: Project[], d: Drop): string | null {
  const order = railProjects().filter((p) => !moving.includes(p));
  order.splice(order.indexOf(d.anchor) + (d.side === "after" ? 1 : 0), 0, ...moving);
  const made = d.make ? freeFleetName() : null;
  const next = new Map<Project, string | null>(order.map((p) => [p, p.fleet ?? null]));
  if (d.fleet !== undefined) for (const p of moving) next.set(p, made ?? d.fleet);
  if (d.make) next.set(d.make, made);
  const size = new Map<string, number>();
  for (const f of next.values()) if (f) size.set(f, (size.get(f) ?? 0) + 1);
  for (const [p, f] of next) if (f && size.get(f) === 1) next.set(p, null);
  const fleets = [...next].filter(([p, f]) => (p.fleet ?? null) !== f).map(([p, f]) => [p.name, f ?? ""]);
  for (const [p, f] of next) p.fleet = f;
  S.projects = order;
  render();
  invoke("reorder_projects", { order: order.map((x) => x.name), fleets }).catch(showError);
  return made;
}

/** The fleet the rail shows open. Undefined before the first render, which does not animate. */
let shownOpen: string | null | undefined;

/**
 * A copy of an open fleet's project chips that shrinks to nothing. It stands
 * for the fleet while it folds, takes no input, and goes when it ends.
 */
function foldOut(block: HTMLElement): HTMLElement {
  const out = h("div", "rail-fleet-open rail-fleet-closing");
  const members = [...block.children].slice(1) as HTMLElement[];
  out.style.setProperty("--h", `${members.reduce((n, el) => n + el.offsetHeight, 0)}px`);
  for (const el of members) {
    const c = el.cloneNode(true) as HTMLElement;
    for (const x of [c, ...c.querySelectorAll<HTMLElement>("*")]) {
      x.removeAttribute("data-project");
      x.removeAttribute("aria-current");
      x.classList.remove("active");
    }
    out.appendChild(c);
  }
  out.inert = true;
  out.setAttribute("aria-hidden", "true");
  out.addEventListener("animationend", (e) => {
    if (e.target === out) out.remove();
  });
  return out;
}

/** True while a chip is in the air. The rail does not rebuild under it. */
let dragging = false;

/**
 * Drag a rail chip or a fleet tile. A press that does not move stays a click.
 * Over a chip, the top and bottom quarters move the drag next to it. The
 * middle half puts a project with a lone chip into a new fleet, or into the
 * chip's fleet. A tile takes a project in its middle. A label at the pointer
 * says what the drop does. Esc cancels.
 */
function dragRail(down: MouseEvent, item: HTMLElement, moving: Project[], fleet: Fleet | null) {
  if (down.button !== 0 || S.projects.length < 2) return;
  const sy = down.clientY;
  const rows = railRows();
  const items = [...$<HTMLElement>("rail").querySelectorAll<HTMLElement>(".rail-item[data-project], .rail-item[data-fleet]")];
  // The rail draws one item per row, in row order.
  if (items.length !== rows.length) return;
  const p = fleet ? null : moving[0];
  let drop: Drop | null = null;
  let ghost: HTMLElement | null = null;
  let preview: HTMLElement | null = null;
  let active = false;

  const itemOf = (r: RailRow) => items[rows.indexOf(r)];
  /** The rows of one fleet: its tile, and its projects when it is open. */
  const fleetRows = (name: string) => rows.filter((r) => (r.kind === "fleet" ? r.f.name : r.inFleet ? r.p.fleet : null) === name);
  const into = (name: string | null) => (name ? `Add to ${name}` : p?.fleet ? `Take out of ${p.fleet}` : "");

  function dropAt(y: number): Drop | null {
    let i = items.findIndex((el) => y < el.getBoundingClientRect().bottom);
    if (i < 0) i = items.length - 1;
    const r = rows[i];
    const box = items[i].getBoundingClientRect();
    const at = (y - box.top) / box.height;
    const zone = at < 0.25 ? "before" : at > 0.75 ? "after" : "into";
    const el = items[i];
    if (fleet) {
      // A fleet moves whole: it lands before or after a lone chip or another fleet.
      const name = r.kind === "fleet" ? r.f.name : r.inFleet ? r.p.fleet! : null;
      if (name === fleet.name) return null;
      if (!name && r.kind === "project") {
        const side = at < 0.5 ? "before" : "after";
        return { anchor: r.p, side, el, mark: side === "before" ? "drop-before" : "drop-after", label: "" };
      }
      const other = S.projects.filter((x) => x.fleet === name);
      const own = fleetRows(name!);
      if (r.kind === "fleet" && at < 0.5) return { anchor: other[0], side: "before", el, mark: "drop-before", label: "" };
      return { anchor: other[other.length - 1], side: "after", el: itemOf(own[own.length - 1]), mark: "drop-after", label: "" };
    }
    if (!p) return null;
    if (r.kind === "project") {
      const q = r.p;
      if (q === p) return null;
      const qFleet = r.inFleet ? q.fleet! : null;
      if (zone === "into") {
        if (qFleet === p.fleet && qFleet) return null;
        if (qFleet) return { anchor: q, side: "after", fleet: qFleet, el, mark: "drop-into", label: into(qFleet) };
        return { anchor: q, side: "after", fleet: null, make: q, el, mark: "drop-into", label: "Make fleet" };
      }
      const label = qFleet !== (p.fleet ?? null) ? into(qFleet) : "";
      return { anchor: q, side: zone, fleet: qFleet, el, mark: zone === "before" ? "drop-before" : "drop-after", label };
    }
    const f = r.f;
    if (zone === "into") {
      if (p.fleet === f.name) return null;
      return { anchor: f.projects[f.projects.length - 1], side: "after", fleet: f.name, el, mark: "drop-into", label: into(f.name) };
    }
    if (zone === "before") return { anchor: f.projects[0], side: "before", fleet: null, el, mark: "drop-before", label: into(null) };
    // Below an open tile is the top of its fleet. Below a folded one is past it.
    if (r.open) return { anchor: f.projects[0], side: "before", fleet: f.name, el, mark: "drop-after", label: p.fleet === f.name ? "" : into(f.name) };
    return { anchor: f.projects[f.projects.length - 1], side: "after", fleet: null, el, mark: "drop-after", label: into(null) };
  }

  /** The drop's mark on the rail. A drop into a fleet shows the tile it makes or grows. */
  function mark() {
    for (const el of items) el.classList.remove("drop-before", "drop-after", "drop-into");
    preview?.remove();
    preview = null;
    if (!drop) return;
    drop.el.classList.add(drop.mark);
    if (drop.mark !== "drop-into" || !p) return;
    const strip = drop.el.querySelector(".fleet-strip");
    if (strip) {
      if (strip.children.length < STRIP_MAX) strip.appendChild((preview = stripPart(accent(p))));
      preview?.classList.add("fleet-preview");
      return;
    }
    preview = fleetTile(freeFleetName(), [accent(drop.anchor), accent(p)]);
    preview.classList.add("fleet-preview");
    drop.el.querySelector(".rail-chip")?.appendChild(preview);
  }

  function place(m: MouseEvent) {
    if (!ghost) return;
    ghost.style.left = `${m.clientX + 8}px`;
    ghost.style.top = `${m.clientY - ghost.offsetHeight / 2}px`;
  }

  const move = (m: MouseEvent) => {
    if (!active) {
      if (Math.abs(m.clientY - sy) < 5) return;
      active = dragging = true;
      item.classList.add("dragging");
      document.body.classList.add("dragging-rail");
      ghost = h("div", "rail-ghost");
      const chip = item.querySelector(".rail-chip")!.cloneNode(true) as HTMLElement;
      chip.classList.remove("active", "attention");
      ghost.append(chip, h("span", "rail-ghost-label"));
      document.body.appendChild(ghost);
    }
    drop = dropAt(m.clientY);
    const label = ghost!.querySelector<HTMLElement>(".rail-ghost-label")!;
    label.textContent = drop?.label ?? "";
    label.hidden = !drop?.label;
    place(m);
    mark();
  };
  const end = () => {
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", up);
    window.removeEventListener("keydown", esc, true);
    if (!active) return false;
    // The click that follows the release would select the project.
    // A release off the chip fires no click, so the guard must not outlive this event.
    const swallow = (c: Event) => c.stopPropagation();
    window.addEventListener("click", swallow, true);
    setTimeout(() => window.removeEventListener("click", swallow, true), 0);
    document.body.classList.remove("dragging-rail");
    item.classList.remove("dragging");
    ghost?.remove();
    const d = drop;
    drop = null;
    mark();
    active = dragging = false;
    return d;
  };
  const up = () => {
    const d = end();
    if (!d) {
      render();
      return;
    }
    const made = applyDrop(moving, d);
    if (made) nameFleet(made);
  };
  const esc = (k: KeyboardEvent) => {
    if (k.key !== "Escape") return;
    k.preventDefault();
    k.stopPropagation();
    end();
    render();
  };
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up);
  window.addEventListener("keydown", esc, true);
}

/**
 * A name field next to a new fleet's tile. The fleet has its "Fleet N" name
 * already: Enter, Esc or a click away keeps what the field holds.
 */
export function nameFleet(name: string) {
  const head = () => document.querySelector<HTMLElement>(`#sidebar-scroll .fleet-pick[data-key="${CSS.escape(`fleet:${name}`)}"]`);
  const tile = head();
  if (!tile) return;
  const box = h("div", "fleet-name");
  const label = h("label", "fleet-name-label", "Fleet name");
  const input = h("input");
  input.type = "text";
  input.value = name;
  input.spellcheck = false;
  input.id = label.htmlFor = "fleet-name-input";
  const hint = h("div", "fleet-name-hint", "Enter or a click away keeps it.");
  box.append(label, input, hint);
  const r = tile.getBoundingClientRect();
  box.style.left = `${r.left + 16}px`;
  box.style.top = `${r.bottom}px`;
  document.body.appendChild(box);
  input.focus();
  input.select();
  let done = false;
  const close = (keep: boolean, blur = false) => {
    const next = input.value.trim();
    const taken = next !== name && S.projects.some((p) => p.fleet === next);
    if (keep && taken && !blur) {
      hint.textContent = `A fleet named ${next} exists. Use another name.`;
      return;
    }
    done = true;
    box.remove();
    head()?.focus();
    if (!keep || taken || !next || next === name) return;
    const members = S.projects.filter((p) => p.fleet === name);
    for (const p of members) p.fleet = next;
    render();
    invoke("reorder_projects", { order: S.projects.map((x) => x.name), fleets: members.map((p) => [p.name, next]) }).catch(showError);
  };
  input.addEventListener("keydown", (e) => {
    e.stopPropagation();
    if (e.key === "Enter") close(true);
    else if (e.key === "Escape") close(false);
  });
  input.addEventListener("blur", () => {
    if (!done) close(true, true);
  });
}
