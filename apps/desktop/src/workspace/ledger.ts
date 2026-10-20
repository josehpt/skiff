/**
 * The Ledger: Skipper's tasks, in a panel at the right of its pane. It shows
 * only while Skipper's pane is the one pane on screen. Focus mode and
 * maximize hide it with the other chrome.
 *
 * Three groups, each hidden when empty: Needs you, Running, Done (done and
 * not reported). One 20px line per row. A plan is one folded row. A click on
 * a task opens its pane; the row menu opens the result or marks it
 * reported, which runs `skiff ledger report`. The rows come read-only from
 * the backend, again on each change to `ledger.db`.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { render } from "../app/renderRequest";
import { skiffAgentSession } from "../app/skiffAgent";
import { sessions } from "../app/state";
import { shownIds } from "../app/stateQueries";
import type { SessionInfo } from "../platform/types";
import { openPath, previewOnScreen } from "../terminal/peek";
import { showError } from "../ui/alerts";
import { ctxMenu } from "../ui/contextMenu";
import { h } from "../ui/dom";
import type { MenuEntry } from "../ui/menu";
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
  state: "sent" | "pending" | "blocked" | "done";
  result_path: string | null;
  reported_at: string | null;
  /** The pane is on another daemon. */
  elsewhere?: boolean;
  result_exists: boolean;
}

const enum Group { Need, Run, Done }
const GROUPS: [Group, string][] = [[Group.Need, "Needs you"], [Group.Run, "Running"], [Group.Done, "Done"]];

/** A task as the panel shows it. */
interface Look {
  r: Entry;
  group: Group;
  /** Why it needs the person. */
  why: string | null;
  /** Its pane, when a click can open it. */
  pane: SessionInfo | null;
  /** The pane name, or what stands for it. */
  where: string;
  gone: boolean;
}

/** A line of the list: a task, or a plan with its shown tasks. */
interface Unit {
  r: Entry;
  group: Group;
  look?: Look;
  kids?: Entry[];
  kept?: Look[];
}

let rows: Entry[] = [];
let error: string | null = null;
let showReported = false;
const unfolded = new Set<string>();

export async function loadLedger() {
  try {
    rows = await invoke<Entry[]>("ledger_rows");
    error = null;
  } catch (e) {
    rows = [];
    error = String(e);
  }
  render();
}

/** Reads the ledger now and on each change. main.ts calls it once. */
export function startLedger() {
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

/** Where a task stands, from its row and its pane. */
function look(r: Entry): Look {
  const s = r.pane_id ? sessions.get(r.pane_id) ?? null : null;
  const short = r.pane_id?.slice(0, 8) ?? "";
  const base = { r, why: null, pane: s, where: s ? taskTitle(s) : short, gone: false };
  if (r.elsewhere) return { ...base, group: r.state === "done" ? Group.Done : Group.Run, pane: null, where: "elsewhere" };
  if (!s) {
    const why = r.state === "done" ? null : "Gone: the pane closed before the task finished.";
    return { ...base, group: r.state === "done" ? Group.Done : Group.Need, why, gone: true };
  }
  if (r.state === "done") return { ...base, group: Group.Done };
  const restarted = r.state !== "pending" && r.pane_gen !== null && !!s.started_at && s.started_at !== r.pane_gen;
  if (restarted) return { ...base, group: Group.Need, why: "Interrupted: the pane restarted as a shell." };
  if (r.state === "pending") return { ...base, group: Group.Need, why: "Not sent yet: the pane was working, or the task waits for a send." };
  if (r.state === "blocked") return { ...base, group: Group.Need, why: "Blocked: the pane needs you." };
  if (s.state === "waiting") return { ...base, group: Group.Need, why: "Waiting: the pane asks for approval." };
  if (s.state === "done") return { ...base, group: Group.Need, why: "Exited: the pane's program ended before the task was done." };
  return { ...base, group: Group.Run };
}

const shows = (r: Entry, all: boolean) => r.state !== "done" || !r.reported_at || all;

/** The lines of the list, in ledger order. */
function units(all: boolean): Unit[] {
  const plans = new Set(rows.filter((r) => r.kind === "plan").map((r) => r.uid));
  const out: Unit[] = [];
  for (const r of rows) {
    if (r.kind === "plan") {
      const kids = rows.filter((k) => k.kind === "task" && k.parent === r.uid);
      if (!kids.length) {
        if (shows(r, all)) out.push({ r, group: r.state === "done" ? Group.Done : Group.Run, kids, kept: [] });
        continue;
      }
      const kept = kids.filter((k) => shows(k, all)).map(look);
      if (kept.length) out.push({ r, group: Math.min(...kept.map((k) => k.group)), kids, kept });
    } else if ((!r.parent || !plans.has(r.parent)) && shows(r, all)) {
      const l = look(r);
      out.push({ r, group: l.group, look: l });
    }
  }
  return out;
}

/** The tasks that need the person: the count on Skipper's row. */
export function ledgerNeeds(): number {
  return rows.filter((r) => r.kind === "task" && r.state !== "done" && look(r).group === Group.Need).length;
}

/** The panel shows while Skipper's pane is the one pane on screen. */
function wanted(): boolean {
  const s = skiffAgentSession();
  if (!s || previewOnScreen()) return false;
  const ids = shownIds().filter((id) => !isSlot(id));
  return ids.length === 1 && ids[0] === s.id;
}

function openPane(s: SessionInfo) {
  revealSession(s.id);
  requestAnimationFrame(() => keepRow(s.id));
}

function report(uids: string[]) {
  invoke("ledger_report", { uids }).then(loadLedger).catch(showError);
}

/**
 * The row menu: Open pane, Open result, Mark reported. A waiting row's title
 * says why. `l` is the task the menu is for; without it, the plan `u`.
 */
function menu(u: Unit, l: Look | undefined, x: number, y: number) {
  const r = l?.r ?? u.r;
  const plan = !l;
  const todo = (plan && u.kids?.length ? u.kids : [r]).filter((k) => k.state === "done" && !k.reported_at);
  const entries: MenuEntry[] = [];
  if (!plan) entries.push({ icon: "code-terminal", label: "Open pane", disabled: !l?.pane || l.gone, run: () => l?.pane && openPane(l.pane) });
  entries.push({ icon: "indicators-square-arrow-out-up-right", label: "Open result", disabled: !r.result_exists, run: () => r.result_path && openPath(r.result_path) });
  entries.push({
    icon: "indicators-check",
    label: todo.length > 1 ? `Mark ${todo.length} reported` : "Mark reported",
    disabled: !todo.length,
    run: () => report(todo.map((k) => k.uid)),
  });
  ctxMenu.open(x, y, l?.why ?? shortTitle(r.task), entries);
}

function dot(group: Group, reported: boolean): HTMLElement {
  return h("span", "lg-dot" + (reported ? " reported" : group === Group.Need ? " need" : group === Group.Run ? " run" : ""));
}

/** The ⋯ button and the right-click both open the menu. */
function withMenu(row: HTMLElement, open: (x: number, y: number) => void) {
  const more = h("button", "lg-more", "⋯");
  more.type = "button";
  more.title = "More";
  more.addEventListener("click", (e) => {
    e.stopPropagation();
    const b = more.getBoundingClientRect();
    open(b.left, b.bottom);
  });
  row.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    open(e.clientX, e.clientY);
  });
  return more;
}

function taskRow(u: Unit, l: Look, kid: boolean): HTMLElement {
  const r = l.r;
  const reported = !!r.reported_at;
  const live = !!l.pane && !l.gone;
  const row = h("div", "lg-row" + (kid ? " kid" : "") + (live ? "" : " dead") + (reported ? " reported" : ""));
  row.title = l.why ? `${r.task}\n\n${l.why}` : r.task;
  const more = withMenu(row, (x, y) => menu(u, l, x, y));
  row.append(dot(l.group, reported), h("span", "lg-title", shortTitle(r.task)), more, h("span", "lg-pane" + (l.gone ? " gone" : ""), l.where));
  if (live) row.addEventListener("click", () => openPane(l.pane!));
  return row;
}

function planRow(u: Unit): HTMLElement[] {
  const r = u.r;
  const kids = u.kids ?? [];
  const open = unfolded.has(r.uid) && !!u.kept?.length;
  const reported = kids.length ? kids.every((k) => !!k.reported_at) : !!r.reported_at;
  const row = h("div", "lg-row plan" + (reported ? " reported" : ""));
  row.title = r.task;
  if (kids.length) {
    row.setAttribute("aria-expanded", String(open));
    const chev = h("span", "lg-chev");
    chev.innerHTML = '<svg width="8" height="8" viewBox="0 0 8 8" fill="none" stroke="currentColor" stroke-width="1.4"><path d="M2.5 1 5.5 4l-3 3"/></svg>';
    row.append(chev);
    row.addEventListener("click", () => {
      if (!unfolded.delete(r.uid)) unfolded.add(r.uid);
      render();
    });
  } else {
    // A plan with no task has nothing to fold: no chevron, no click.
    row.classList.add("dead");
    row.append(dot(u.group, reported));
  }
  if (u.group === Group.Need) row.append(dot(Group.Need, false));
  const more = withMenu(row, (x, y) => menu(u, undefined, x, y));
  const done = kids.filter((k) => k.state === "done").length;
  row.append(h("span", "lg-title", shortTitle(r.task)), more, h("span", "lg-pane", kids.length ? `${done}/${kids.length}` : ""));
  const out: HTMLElement[] = [row];
  if (open) for (const k of u.kept ?? []) out.push(taskRow(u, k, true));
  return out;
}

/** What the list draws. The panel is rebuilt only when it changes. */
function signature(us: Unit[]): string {
  return JSON.stringify([
    error, showReported, [...unfolded],
    us.map((u) => [u.r.id, u.r.state, u.r.reported_at, u.r.result_exists, u.group, u.look ? [u.look.where, u.look.why, u.look.gone] : null,
      u.kept?.map((k) => [k.r.id, k.r.state, k.r.reported_at, k.r.result_exists, k.group, k.where, k.why, k.gone])]),
  ]);
}

/** Runs on each render, before the modes: shows, hides and fills the panel. */
export function renderLedger() {
  const panel = document.getElementById("ledger");
  if (!panel) return;
  const show = wanted();
  panel.hidden = !show;
  if (!show) return;
  const us = units(showReported);
  const sig = signature(us);
  if (panel.dataset.sig === sig) return;
  panel.dataset.sig = sig;
  const list = h("div", "lg-list");
  for (const [g, name] of GROUPS) {
    const part = us.filter((u) => u.group === g);
    if (!part.length) continue;
    list.append(h("div", "lg-group", name));
    for (const u of part) list.append(...(u.look ? [taskRow(u, u.look, false)] : planRow(u)));
  }
  if (error) list.append(h("div", "lg-empty", error));
  else if (!us.length) list.append(h("div", "lg-empty", "Nothing open."));
  const foot = h("button", "lg-foot", showReported ? "Hide reported" : "Show reported");
  foot.type = "button";
  foot.addEventListener("click", () => {
    showReported = !showReported;
    render();
  });
  list.append(foot);
  const top = panel.querySelector(".lg-list")?.scrollTop ?? 0;
  panel.replaceChildren(h("div", "lg-head", "Ledger"), list);
  list.scrollTop = top;
}
