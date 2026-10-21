/**
 * The Ledger: Skipper's tasks, in a panel at the right of its pane. It shows
 * only while Skipper's pane is the one pane on screen. Focus mode keeps it;
 * maximize hides it with the other chrome.
 *
 * Three sections, each shown when it has work. Working: an agent is on the
 * work now (a sent task whose pane works or ended its turn), in send order.
 * Open: the work is not closed and no agent is on it: the rows that need the
 * person first, then done tasks, then the rest. A row that needs the person
 * turns amber. Finished: the work Skipper closed on the person's word, newest
 * first, the last 100. A plan is a head row with its tasks always open under
 * it, and it counts as one item. Rows slide to their new place, flash on a
 * change and fade in; a close slides the work down into Finished.
 *
 * Ctrl+Shift+Right from Skipper's pane gives the keys to the panel, and opens
 * a folded one. Ctrl+Shift+Up/Down, or the plain arrows, step the lit row.
 * Enter does the row's click. Ctrl+Shift+Left or Esc gives the keys back to
 * Skipper's pane. The lit row stays across redraws.
 *
 * A click on a row does the main action for its state: open the pane or the
 * result. Small icons at the right edge do the rest, only when they apply:
 * the pane or the result. The panel never writes: only Skipper changes rows.
 * No row has a menu. The rows come read-only from the backend, again on each
 * change to `ledger.db`.
 *
 * The person can fold the panel to a rail: a chevron, and the amber count of
 * the tasks that need them. The fold stays in localStorage on this machine.
 * A fold or an unfold slides the width; a saved fold at start does not.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { keyLabel } from "../app/keys";
import { render } from "../app/renderRequest";
import { skiffAgentSession } from "../app/skiffAgent";
import { S, sessions } from "../app/state";
import { shownIds } from "../app/stateQueries";
import type { SessionInfo } from "../platform/types";
import { openPath, previewToken } from "../terminal/peek";
import { holdFits, releaseFits } from "../terminal/terminal";
import { panes } from "../terminal/terminalState";
import type { Action } from "../app/keys";
import { h } from "../ui/dom";
import { isSlot } from "./layoutSlots";
import { taskTitle } from "./model";
import { keepRow } from "./selection";
import { revealSession } from "./view";

/** A ledger row as `ledger_rows` returns it. */
interface Entry {
  id: number;
  uid: string;
  kind: "task" | "plan";
  parent: string | null;
  pane_id: string | null;
  pane_gen: number | null;
  task: string;
  sent_at: string;
  /** The short title Skipper gave it. */
  title: string | null;
  state: "sent" | "pending" | "blocked" | "done";
  result_path: string | null;
  updated_at: string;
  /** When the person closed the work. Only a plan, or a task with no plan, has it. */
  closed_at: string | null;
  /** The pane is on another daemon. */
  elsewhere?: boolean;
  result_exists: boolean;
}

/** Where a task stands, from its row and its pane. */
type State = "pending" | "sent" | "approval" | "blocked" | "interrupted" | "gone" | "exited" | "done";

/** The states that need the person, and the word that takes the pane name's place. */
const NEED: Partial<Record<State, string>> = { approval: "asks approval", blocked: "blocked", interrupted: "restarted", gone: "pane gone", exited: "exited" };
const WHY: Partial<Record<State, string>> = {
  approval: "The agent asks approval in its pane.",
  blocked: "The task is blocked. Skipper needs an answer.",
  interrupted: "The pane restarted as a shell. The task stopped.",
  gone: "The pane closed before the task finished.",
  exited: "The pane's program ended before the task was done.",
};
/** The order of the needs, most urgent first: a plan shows the first its tasks have. */
const URGENT: State[] = ["approval", "blocked", "interrupted", "gone", "exited"];

/** The order in Open: needs the person, then done, then the rest. */
const rank = (st: State) => (NEED[st] ? 0 : st === "done" ? 1 : 2);

/** Finished keeps this many items. A plan counts as one. The backend reads no more. */
const KEEP = 100;

/** A task as the panel shows it. */
interface Look {
  r: Entry;
  st: State;
  /** Its pane, when a click can open it. */
  pane: SessionInfo | null;
  /** The pane name, or what stands for it. */
  where: string;
}

/** An item of the list: a task, or a plan with its tasks. */
interface Unit {
  r: Entry;
  st: State;
  look?: Look;
  kids?: Look[];
  /** When the work was closed. `null`: it is open. */
  closed: string | null;
  /** An agent is on one of its tasks now. */
  working: boolean;
}

let rows: Entry[] = [];
let error: string | null = null;
let loaded = false;

export async function loadLedger() {
  try {
    rows = await invoke<Entry[]>("ledger_rows");
    error = null;
  } catch (e) {
    rows = [];
    error = String(e);
  }
  loaded = true;
  render();
}

/** Reads the ledger now and on each change. main.ts calls it once. */
export function startLedger() {
  const panel = document.getElementById("ledger");
  panel?.addEventListener("keydown", rowKey);
  void listen("skiff:ledger-changed", () => void loadLedger());
  void loadLedger();
}

/**
 * The short title: the first sentence of the task text. It ends at the first
 * ". ", "; " or line break outside parentheses, and is cut at a space before
 * 80 characters. The row's ellipsis cuts the rest; the tooltip has it all.
 */
export function shortTitle(text: string): string {
  const t = text.trim();
  let depth = 0;
  let end = t.length;
  for (let i = 0; i < t.length; i++) {
    const c = t[i];
    if (c === "(") depth++;
    else if (c === ")") depth = Math.max(0, depth - 1);
    else if (c === "\n" || (depth === 0 && (c === "." || c === ";") && (i + 1 === t.length || t[i + 1] === " "))) {
      end = i;
      break;
    }
  }
  const s = t.slice(0, end).trim();
  if (s.length <= 80) return s;
  const cut = s.lastIndexOf(" ", 80);
  return s.slice(0, cut > 0 ? cut : 80);
}

/** The title Skipper gave the row, else the one the task text gives. */
const titleOf = (r: Entry) => r.title?.trim() || shortTitle(r.task);

function look(r: Entry): Look {
  const s = r.pane_id ? sessions.get(r.pane_id) ?? null : null;
  const base = { r, pane: s, where: s ? taskTitle(s) : r.pane_id?.slice(0, 8) ?? "" };
  if (r.elsewhere) return { ...base, st: r.state === "done" || r.state === "pending" ? r.state : "sent", pane: null, where: "elsewhere" };
  if (r.state === "done") return { ...base, st: "done" };
  if (!s) return { ...base, st: "gone" };
  const restarted = r.state !== "pending" && r.pane_gen !== null && !!s.started_at && s.started_at !== r.pane_gen;
  if (restarted) return { ...base, st: "interrupted" };
  if (r.state === "pending" || r.state === "blocked") return { ...base, st: r.state };
  if (s.state === "waiting") return { ...base, st: "approval" };
  if (s.state === "done") return { ...base, st: "exited" };
  return { ...base, st: "sent" };
}

/** A plan's state from its tasks: the most urgent need shows. */
function planState(kids: Look[]): State {
  for (const st of URGENT) if (kids.some((k) => k.st === st)) return st;
  if (kids.every((k) => k.st === "done")) return "done";
  if (kids.every((k) => k.st === "pending")) return "pending";
  return "sent";
}

/** Every item, in ledger order. */
function units(): Unit[] {
  const plans = new Set(rows.filter((r) => r.kind === "plan").map((r) => r.uid));
  const out: Unit[] = [];
  for (const r of rows) {
    if (r.kind === "plan") {
      const kids = rows.filter((k) => k.kind === "task" && k.parent === r.uid).map(look);
      if (!kids.length) {
        // A plan with no task stands by its own state.
        const st: State = r.state === "done" || r.state === "pending" || r.state === "blocked" ? r.state : "sent";
        out.push({ r, st, kids, closed: r.closed_at, working: false });
        continue;
      }
      out.push({ r, st: planState(kids), kids, closed: r.closed_at, working: kids.some((k) => k.st === "sent") });
    } else if (!r.parent || !plans.has(r.parent)) {
      const l = look(r);
      out.push({ r, st: l.st, look: l, closed: r.closed_at, working: l.st === "sent" });
    }
  }
  return out;
}

/** The three sections. */
interface Sections {
  working: Unit[];
  open: Unit[];
  finished: Unit[];
}

/** Working and Open in send order, Open by its rank first; Finished newest closed first, the last [`KEEP`]. */
function sections(us: Unit[]): Sections {
  const live = us.filter((u) => !u.closed);
  return {
    working: live.filter((u) => u.working),
    open: live.filter((u) => !u.working).sort((a, b) => rank(a.st) - rank(b.st) || a.r.id - b.r.id),
    finished: us
      .filter((u) => u.closed)
      .sort((a, b) => b.closed!.localeCompare(a.closed!) || b.r.id - a.r.id)
      .slice(0, KEEP),
  };
}

/** The tasks of work not closed that need the person. */
function needing(us: Unit[]): number {
  return us
    .filter((u) => !u.closed)
    .flatMap((u) => u.kids ?? (u.look ? [u.look] : []))
    .filter((l) => NEED[l.st]).length;
}

/** The count on Skipper's row. */
export function ledgerNeeds(): number {
  return needing(units());
}

/** The panel shows while Skipper's pane is the one pane on screen. A preview a row opened keeps it; any other preview hides it. */
function wanted(): boolean {
  const s = skiffAgentSession();
  const t = previewToken();
  if (!s || (t && t !== ours)) return false;
  const ids = shownIds().filter((id) => !isSlot(id));
  return ids.length === 1 && ids[0] === s.id;
}

/** The panel is on screen: focus mode keeps it, maximize hides it. The fold acts only then. */
export function ledgerShown(): boolean {
  return wanted() && !S.maximized;
}

const FOLD_KEY = "skiff.ledgerFolded";
let folded = (() => {
  try {
    return localStorage.getItem(FOLD_KEY) === "1";
  } catch {
    return false;
  }
})();

export const ledgerFolded = () => folded;

/** Folds the panel to its rail, or opens it. Nothing while the panel is not on screen. */
export function toggleLedger() {
  if (!ledgerShown()) return;
  folded = !folded;
  try {
    localStorage.setItem(FOLD_KEY, folded ? "1" : "0");
  } catch {
    /* the fold lasts for this run */
  }
  slide();
  render();
}

/** As focus mode and maximize: 180ms, ease-out. */
const SLIDE_MS = 180;
/** A fold in progress: the open panel's rows stay, clipped, until the rail takes their place. */
let folding = false;
let slideEnd = 0;

/**
 * Slides the width to the new fold. Skipper's terminal keeps its size
 * during the slide and fits once at the end.
 */
function slide() {
  const panel = document.getElementById("ledger");
  if (!panel || still()) return;
  panel.classList.add("sliding");
  folding = folded;
  holdFits();
  clearTimeout(slideEnd);
  // A frame after the transition, so the fit reads the final width.
  slideEnd = window.setTimeout(() => {
    panel.classList.remove("sliding");
    folding = false;
    render();
    releaseFits();
  }, SLIDE_MS + 40);
}

/** The next draw of the open panel flashes the first row that needs the person. */
let flashNext = false;

function flashNeed(panel: HTMLElement) {
  const r = panel.querySelector<HTMLElement>(".lg-list .lg-row.need");
  if (!r) return;
  r.scrollIntoView({ block: "nearest" });
  r.classList.remove("flash");
  void r.offsetWidth;
  r.classList.add("flash");
}

function openPane(s: SessionInfo) {
  revealSession(s.id);
  requestAnimationFrame(() => keepRow(s.id));
}

/** The task's pane is live, so a click can open it. */
const opens = (l: Look) => !!l.pane && l.st !== "gone";

/** The row icons, at the stroke weight of the tree icons. */
const ICON = {
  fold: '<svg viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.3"><path d="M3.5 1.5 7 5 3.5 8.5"/></svg>',
  unfold: '<svg viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.3"><path d="M6.5 1.5 3 5l3.5 3.5"/></svg>',
  pane: '<svg viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.3"><rect x=".65" y="1.15" width="8.7" height="7.7"/><path d="M2.6 3.8 4.2 5 2.6 6.2M5.2 6.4h2"/></svg>',
  result: '<svg viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.3"><path d="M2 .65h4L8.35 3v6.35H2z"/><path d="M5.8.65V3.2h2.55"/></svg>',
};

/** What a row does: its click, its tooltip, and the icons at its right edge. */
interface Acts {
  click: (() => void) | null;
  tip: string;
  icons: [keyof typeof ICON, string, () => void][];
}

/** The preview a Ledger row opened. The panel stays beside it. */
let ours = 0;

const result = (r: Entry) =>
  r.result_exists && r.result_path
    ? () => {
        openPath(r.result_path!);
        ours = previewToken();
      }
    : null;

/**
 * A task's actions. Running, approval, blocked, pending, exited: the click
 * opens the pane. Restarted: the pane, and a result icon when the file
 * exists. Gone: the result, else nothing. Done: the result, with a pane
 * icon. Finished: the result only.
 */
function taskActs(l: Look, fin: boolean): Acts {
  const r = l.r;
  const res = result(r);
  const pane = opens(l) ? () => openPane(l.pane!) : null;
  if (fin) return { click: res, tip: res ? "Open result" : "No result file", icons: [] };
  if (l.st === "done") {
    const icons: Acts["icons"] = [];
    if (pane) icons.push(["pane", "Open pane", pane]);
    return { click: res, tip: res ? "Open result" : "No result file yet", icons };
  }
  if (l.st === "gone") return { click: res, tip: res ? "Open result" : "The pane is gone. No result yet.", icons: [] };
  const icons: Acts["icons"] = l.st === "interrupted" && res ? [["result", "Open result", res]] : [];
  return { click: pane, tip: pane ? "Open pane" : "The pane is on another daemon", icons };
}

/** A ledger time (UTC, ISO) as local time: "Oct 20 21:37". */
function stamp(iso: string): string {
  const d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  const day = d.toLocaleDateString([], { month: "short", day: "numeric" });
  return `${day} ${d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false })}`;
}

/** The row's times for its tooltip: sent, last change, and closed when it is. */
function times(r: Entry): string {
  const t = [`Sent ${stamp(r.sent_at)}`, `Updated ${stamp(r.updated_at)}`];
  if (r.closed_at) t.push(`Closed ${stamp(r.closed_at)}`);
  return t.join("\n");
}

/** One 20px line, with its actions. */
function line(r: Entry, st: State, label: string, a: Acts, o: { plan?: boolean; kid?: boolean; fin?: boolean }): HTMLElement {
  const need = !o.fin && !!NEED[st];
  const row = h("div", `lg-row s-${st}` + (need ? " need" : "") + (o.plan ? " plan" : "") + (o.kid ? " kid" : "") + (o.fin ? " fin" : "") + (a.click ? "" : " dead"));
  row.dataset.key = r.uid;
  row.tabIndex = -1;
  row.dataset.look = o.fin ? "fin" : `${st}:${label}`;
  const why = need && WHY[st] ? `${WHY[st]}\n` : "";
  row.title = `${why}${a.tip}\n\n${r.task}\n\n${times(r)}`;
  const acts = h("span", "lg-acts");
  for (const [name, tip, run] of a.icons) {
    const b = h("button", "lg-ico");
    b.type = "button";
    b.innerHTML = ICON[name];
    b.title = tip;
    b.setAttribute("aria-label", tip);
    b.addEventListener("click", (e) => {
      e.stopPropagation();
      run();
    });
    acts.append(b);
  }
  row.append(h("span", "lg-dot"), h("span", "lg-title", titleOf(r)), h("span", o.plan ? "lg-n" : "lg-pane", label), acts);
  if (a.click) row.addEventListener("click", a.click);
  return row;
}

function taskRow(l: Look, kid: boolean, fin: boolean): HTMLElement {
  return line(l.r, l.st, (!fin && NEED[l.st]) || l.where, taskActs(l, fin), { kid, fin });
}

function unitRows(u: Unit, fin: boolean): HTMLElement[] {
  if (u.look) return [taskRow(u.look, false, fin)];
  const kids = u.kids ?? [];
  const n = kids.length ? `${kids.filter((k) => k.st === "done").length}/${kids.length}` : "";
  // A plan row is a head for its tasks: its click opens plan.md.
  const res = result(u.r);
  const row = line(u.r, fin ? "done" : u.st, n, { click: res, tip: res ? "Open plan.md" : "No plan.md yet", icons: [] }, { plan: true, fin });
  return [row, ...kids.map((k) => taskRow(k, true, fin))];
}

/** What the list draws. The panel is rebuilt only when it changes. */
function signature(x: Sections): string {
  const look = (l: Look) => [l.r.id, l.r.state, l.r.title, l.r.task, l.r.updated_at, l.r.closed_at, l.r.result_exists, l.st, l.where, opens(l)];
  const unit = (u: Unit) => [u.r.id, u.st, u.r.title, u.r.task, u.r.updated_at, u.r.closed_at, u.r.result_exists, u.look ? look(u.look) : null, u.kids?.map(look)];
  return JSON.stringify([error, x.working.map(unit), x.open.map(unit), x.finished.map(unit)]);
}

const still = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

/**
 * Moves the rows of `list` from their places in `before`: a row that stays
 * slides from its old place, a row with a new look flashes, a new row fades
 * in, a row that left fades up and out. A row that moved into Finished
 * slides slower and does not flash.
 */
function animate(list: HTMLElement, before: Map<string, { top: number; el: HTMLElement }>) {
  const after = new Set<string>();
  for (const r of list.querySelectorAll<HTMLElement>(".lg-row[data-key]")) {
    const k = r.dataset.key!;
    after.add(k);
    const b = before.get(k);
    if (!b) {
      r.classList.add("enter");
      continue;
    }
    const intoFin = r.classList.contains("fin") && !b.el.classList.contains("fin");
    if (b.el.dataset.look !== r.dataset.look && !intoFin) r.classList.add("flash");
    const dy = b.top - r.offsetTop;
    if (!dy) continue;
    r.style.transform = `translateY(${dy}px)`;
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        r.classList.add("moving");
        if (intoFin) r.classList.add("long");
        r.style.transform = "";
      }),
    );
  }
  for (const [k, b] of before) {
    if (after.has(k)) continue;
    const g = b.el.cloneNode(true) as HTMLElement;
    g.classList.remove("moving", "long", "enter", "flash");
    g.classList.add("ghost");
    g.removeAttribute("data-key");
    g.style.top = `${b.top}px`;
    g.style.transform = "";
    list.append(g);
    requestAnimationFrame(() => requestAnimationFrame(() => g.classList.add("out")));
    setTimeout(() => g.remove(), 420);
  }
}

/** The panel's list was on screen at the last render, so a change can move. */
let drawn = false;

/** A click does not take the keys: the terminal that has them keeps them. */
const keepKeys = (b: HTMLElement) => b.addEventListener("mousedown", (e) => e.preventDefault());

function foldButton(name: "fold" | "unfold"): HTMLButtonElement {
  const b = h("button", "lg-fold") as HTMLButtonElement;
  b.type = "button";
  b.innerHTML = ICON[name];
  keepKeys(b);
  b.addEventListener("click", toggleLedger);
  return b;
}

/** A tooltip with the fold key, which `[keys]` can change. */
function foldTip(b: HTMLElement, what: string) {
  const k = keyLabel("ledger");
  b.title = k ? `${what} (${k})` : what;
  b.setAttribute("aria-label", what);
}

/** The folded panel: a chevron that opens it, and the amber count under it. */
function renderRail(panel: HTMLElement) {
  let rail = panel.querySelector<HTMLElement>(".lg-rail");
  if (!rail) {
    const count = h("button", "lg-rail-need");
    (count as HTMLButtonElement).type = "button";
    keepKeys(count);
    count.addEventListener("click", () => {
      flashNext = true;
      toggleLedger();
    });
    rail = h("div", "lg-rail");
    rail.append(foldButton("unfold"), count);
    panel.replaceChildren(rail);
  }
  foldTip(rail.querySelector(".lg-fold")!, "Show the Ledger");
  const count = rail.querySelector<HTMLElement>(".lg-rail-need")!;
  const n = needing(units());
  count.hidden = !n;
  count.textContent = String(n);
  count.title = `${n} ledger ${n === 1 ? "task needs" : "tasks need"} you`;
  count.setAttribute("aria-label", count.title);
}

/** Runs on each render, before the modes: shows, hides and fills the panel. */
export function renderLedger() {
  const panel = document.getElementById("ledger");
  if (!panel) return;
  const show = wanted();
  panel.hidden = !show;
  if (!show) {
    drawn = false;
    setKeys(false);
    return;
  }
  panel.classList.toggle("folded", folded);
  if (folded && !folding) {
    drawn = false;
    setKeys(false);
    renderRail(panel);
    return;
  }
  const us = units();
  const x = sections(us);
  const sig = signature(x);
  let list = panel.querySelector<HTMLElement>(".lg-list");
  let need = panel.querySelector<HTMLElement>(".lg-need");
  const fold = panel.querySelector<HTMLElement>(".lg-head .lg-fold");
  if (fold) foldTip(fold, "Hide the Ledger");
  if (drawn && list && panel.dataset.sig === sig) return;
  panel.dataset.sig = sig;

  if (!list || !need) {
    need = h("button", "lg-need");
    (need as HTMLButtonElement).type = "button";
    need.title = "Go to the first row that needs you";
    need.addEventListener("click", () => flashNeed(panel));
    const head = h("div", "lg-head");
    const fold = foldButton("fold");
    foldTip(fold, "Hide the Ledger");
    head.append(h("span", "lg-name", "Ledger"), need, fold);
    list = h("div", "lg-list");
    panel.replaceChildren(head, list);
  }
  const n = needing(us);
  need.hidden = !n;
  need.textContent = `${n} need${n === 1 ? "s" : ""} you`;

  const move = drawn && loaded && !still();
  const before = new Map<string, { top: number; el: HTMLElement }>();
  if (move) for (const r of list.querySelectorAll<HTMLElement>(".lg-row[data-key]")) before.set(r.dataset.key!, { top: r.offsetTop, el: r });

  const out: HTMLElement[] = [];
  if (error) out.push(h("div", "lg-empty", error));
  for (const [label, part, fin] of [["Working", x.working, false], ["Open", x.open, false], ["Finished", x.finished, true]] as const) {
    if (!part.length) continue;
    out.push(h("div", "lg-sec", label));
    for (const u of part) out.push(...unitRows(u, fin));
  }
  list.replaceChildren(...out);
  if (move) animate(list, before);
  if (keys) light();
  drawn = loaded;
  if (flashNext) {
    flashNext = false;
    flashNeed(panel);
  }
}

// ── Keys ────────────────────────────────────────────────────────────────────

/** The keys are in the panel. The panel wears the focused pane's frame meanwhile. */
let keys = false;

function setKeys(on: boolean) {
  keys = on;
  // One frame on screen: Skipper's pane drops its frame while the panel has the keys.
  document.body.classList.toggle("ledger-keys", on);
}
/** The lit row's uid. */
let lit: string | null = null;

const rowEls = () => [...document.querySelectorAll<HTMLElement>("#ledger .lg-list .lg-row[data-key]:not(.ghost)")];

/** Lights the row `lit` names, else the first that needs the person, else the first, and gives it the keys. */
function light() {
  const rs = rowEls();
  for (const r of rs) r.classList.remove("lit");
  const r = rs.find((x) => x.dataset.key === lit) ?? rs.find((x) => x.classList.contains("need")) ?? rs[0];
  if (!r) return;
  lit = r.dataset.key!;
  r.classList.add("lit");
  r.focus({ preventScroll: true });
  r.scrollIntoView({ block: "nearest" });
}

function step(dir: 1 | -1) {
  const rs = rowEls();
  if (!rs.length) return;
  const i = rs.findIndex((x) => x.dataset.key === lit);
  lit = rs[i < 0 ? 0 : (i + dir + rs.length) % rs.length].dataset.key!;
  light();
}

/** Ctrl+Shift+Right: the keys go to the panel. A folded panel opens first. */
function enter() {
  if (folded) toggleLedger();
  setKeys(true);
  light();
}

/** Ctrl+Shift+Left or Esc: the keys go back to Skipper's pane. */
function leave() {
  setKeys(false);
  for (const r of rowEls()) r.classList.remove("lit");
  const s = skiffAgentSession();
  if (s) panes.get(s.id)?.term.focus();
}

/** Plain keys on a lit row: the arrows step, Enter clicks, Esc leaves. */
function rowKey(e: KeyboardEvent) {
  if (!keys || e.ctrlKey || e.altKey || e.metaKey || e.shiftKey) return;
  const r = (e.target as HTMLElement).closest<HTMLElement>(".lg-row[data-key]");
  if (!r) return;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") step(e.key === "ArrowDown" ? 1 : -1);
  else if (e.key === "Enter") r.click();
  else if (e.key === "Escape") leave();
  else return;
  e.preventDefault();
}

// The keys left the panel (a click, F6, a pane, an opened row): the light goes.
document.addEventListener("focusin", (e) => {
  if (!keys || document.getElementById("ledger")?.contains(e.target as Node)) return;
  setKeys(false);
  for (const r of rowEls()) r.classList.remove("lit");
});

/**
 * The list keys while the Ledger is on screen, and the maximize key on its lit
 * row. True when the Ledger took the key. From a project or fleet row, Ctrl+Shift+Right keeps its list meaning.
 */
export function ledgerKey(a: Action): boolean {
  if (!ledgerShown()) return false;
  if (keys) {
    if (a === "session-next" || a === "session-prev") step(a === "session-next" ? 1 : -1);
    else if (a === "list-project") leave();
    // Ctrl+Shift+Enter opens the lit row, as Enter does. No pane to maximize from here.
    else if (a === "maximize") rowEls().find((x) => x.dataset.key === lit)?.click();
    else return a === "list-back";
    return true;
  }
  if (a !== "list-back") return false;
  const el = document.activeElement;
  if (el?.classList.contains("project-pick") || el?.classList.contains("fleet-pick")) return false;
  enter();
  return true;
}
