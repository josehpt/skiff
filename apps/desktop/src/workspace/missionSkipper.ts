/**
 * The Skipper lane in Mission control: the watcher, when it does not run
 * well, and the open tasks in Skipper's ledger. It shows, first, only when one
 * of the two needs a look; a quiet board has no lane. missionControl.ts puts
 * it on the board and moves the light through it. This file reads the ledger
 * and the watch lock while the board is open, draws the lane, and runs the
 * recovery actions through the `skiff ledger` CLI.
 */

import { invoke } from "@tauri-apps/api/core";

import type { SessionInfo } from "../platform/types";
import { sessions } from "../app/state";
import { place } from "../app/stateQueries";
import {
  isSkiffAgent,
  openWatchLog,
  restartSkiffWatch,
  restarting,
  skiffAgentFolder,
  skiffAgentName,
  skiffAgentSession,
  skiffWatch,
  watchRestart,
  type WatchState,
} from "../app/skiffAgent";
import { h } from "../ui/dom";
import { showError } from "../ui/alerts";
import { ctxMenu } from "../ui/contextMenu";
import type { MenuEntry } from "../ui/menu";
import { promptAction } from "../ui/confirm";
import { actionDialog } from "../ui/dialogParts";
import { openPath } from "../terminal/peek";
import { agentName, taskTitle } from "./model";
import { closeMission, goToSession } from "./missionMotion";

/** A row of `skiff ledger ls --json`. */
export interface LedgerRow {
  uid: string;
  kind: string;
  pane_id: string | null;
  project: string;
  task: string;
  sent_at: string;
  state: string;
  result_path: string | null;
  ref?: string | null;
  /** The pane still exists. Absent when the daemon did not answer. */
  live?: boolean;
  /** The pane restarted since the send. */
  interrupted?: boolean;
}

/** An open task's state as the lane shows it: the ledger's, or gone or interrupted. */
export type TaskState = "gone" | "interrupted" | "blocked" | "pending" | "sent";

/** What the watcher tile says. Null: the watcher runs well, or its state is not known yet. */
export type WatchTile = "stale" | "stopped" | "ledger" | "undelivered" | "restarting" | "restart-failed";

export type Section = "look" | "pending" | "running";

const SECTIONS: [Section, string][] = [
  ["look", "Needs a look"],
  ["pending", "Pending"],
  ["running", "Running"],
];

/** The open state of a ledger row; null for a plan or a done task. */
export function taskState(r: LedgerRow): TaskState | null {
  if (r.kind !== "task" || r.state === "done") return null;
  if (r.interrupted) return "interrupted";
  if (r.live === false) return "gone";
  return r.state === "sent" || r.state === "pending" || r.state === "blocked" ? r.state : null;
}

export function sectionOf(st: TaskState): Section {
  return st === "pending" ? "pending" : st === "sent" ? "running" : "look";
}

/**
 * The watcher tile for the watcher's state. `held`: a process holds the watch
 * lock. A watcher that holds the lock and did not register is stale: the
 * app's own start does nothing while the lock is held.
 */
export function watchTile(state: WatchState, failure: string | undefined, held: boolean | null, restartNow: boolean, restartError: string | null): WatchTile | null {
  if (state === "runs") return null;
  if (restartNow) return "restarting";
  if (restartError) return "restart-failed";
  if (state === "failed") return failure === "undelivered" ? "undelivered" : "ledger";
  if (state === "missing") return held ? "stale" : "stopped";
  return null;
}

/** The lane shows when the watcher needs a look or a task is open. */
export const laneShows = (watch: WatchTile | null, open: number) => !!watch || open > 0;

/** Enter on a tile: go to a live task's pane, open a gone task's menu. */
export const enterFor = (st: TaskState): "go" | "menu" => (st === "gone" ? "menu" : "go");

/** The tile key of a task. The watcher tile's key is `watch`. */
const taskKey = (uid: string) => `task:${uid}`;
export const isSkipperKey = (k: string | null | undefined) => !!k && (k === "watch" || k.startsWith("task:"));

interface Holder {
  pid: number;
  socket: string;
  started_at: number;
}

interface OpenTask {
  key: string;
  row: LedgerRow;
  state: TaskState;
  /** The task's pane, while it runs. */
  s: SessionInfo | null;
}

export interface SkipperView {
  watch: WatchTile | null;
  tasks: OpenTask[];
}

let rows: LedgerRow[] = [];
let lock: { held: boolean; holder: Holder | null } | null = null;
let readAt = 0;
/** The read under way. A second read waits for it, so an action redraws from fresh rows. */
let reading: Promise<void> | null = null;
/** Why the last action on a task failed, or what it left, shown on its hint line. */
const notes = new Map<string, string>();
/** Each live task's screen: its last rows of text. */
const screens = new Map<string, HTMLPreElement>();
/** The board's own redraw, after an action changed the ledger. */
let changed: () => void = () => {};

/** How often the open board reads the ledger and the watch lock. */
const READ_EVERY = 3000;
/** Rows of a task's screen. */
export const TASK_ROWS = 4;

/** The head's status: one element for the board's life, so a screen reader hears only a change. */
const status = h("span", "mission-watch-state");
status.setAttribute("role", "status");
status.setAttribute("aria-live", "polite");

const withTimeout = <T>(p: Promise<T>, ms: number) =>
  Promise.race([p, new Promise<T>((_, no) => window.setTimeout(() => no(new Error("timed out")), ms))]);

/** The pane a row names: by full id, or by the prefix an old ledger kept. */
function paneOf(id: string | null): SessionInfo | null {
  if (!id) return null;
  return sessions.get(id) ?? [...sessions.values()].find((x) => x.id.startsWith(id)) ?? null;
}

/** Reads the ledger and the watch lock. Only while the board is open. */
async function read(): Promise<void> {
  if (reading) {
    await reading;
    return readOnce();
  }
  return readOnce();
}

function readOnce(): Promise<void> {
  reading ??= load().finally(() => (reading = null));
  return reading;
}

async function load() {
  if (!skiffAgentFolder()) {
    rows = [];
    lock = null;
    return;
  }
  readAt = Date.now();
  const [r, l] = await Promise.allSettled([
    withTimeout(invoke<LedgerRow[]>("skiff_open_work"), 5000),
    withTimeout(invoke<{ held: boolean; holder: Holder | null }>("skiff_watch_status"), 5000),
  ]);
  if (r.status === "fulfilled" && Array.isArray(r.value)) rows = r.value;
  else if (r.status === "rejected") console.error("skiff ledger ls:", r.reason);
  lock = l.status === "fulfilled" ? l.value : null;
}

export const skipper = {
  /** Reads now; the board awaits it before it shows. */
  read,
  /** On the board's tick: reads again now and then. */
  poll() {
    if (Date.now() - readAt >= READ_EVERY) void read().then(() => changed());
  },
  onChange(f: () => void) {
    changed = f;
  },
  clear() {
    screens.clear();
    notes.clear();
    readAt = 0;
  },
};

/** The lane's watcher tile and tasks now, or null when it does not show. */
export function skipperView(): SkipperView | null {
  if (!skiffAgentFolder()) return null;
  const me = skiffAgentSession();
  let watch: WatchTile | null = null;
  if (me) {
    const w = skiffWatch(me).state;
    if (w === "runs") watchRestart.error = null;
    watch = watchTile(w, me.watch?.failure?.kind, lock?.held ?? null, restarting(), watchRestart.error);
  }
  const tasks: OpenTask[] = [];
  for (const row of rows) {
    let state = taskState(row);
    if (!state) continue;
    const s = paneOf(row.pane_id);
    if (s && isSkiffAgent(s.id)) continue;
    // The pane went away since the ledger read.
    if (!s && row.live === true) state = "gone";
    tasks.push({ key: taskKey(row.uid), row, state, s });
  }
  const rank = (t: OpenTask) => SECTIONS.findIndex(([k]) => k === sectionOf(t.state));
  tasks.sort((a, b) => rank(a) - rank(b));
  return laneShows(watch, tasks.length) ? { watch, tasks } : null;
}

/** The tile keys in board order. */
export function skipperKeys(v: SkipperView): string[] {
  return [...(v.watch ? ["watch"] : []), ...v.tasks.map((t) => t.key)];
}

/** The lane's structure: a change rebuilds it. */
export function skipperSignature(v: SkipperView | null): string {
  if (!v) return "";
  return `skipper:${v.watch ?? ""}:` + v.tasks.map((t) => `${t.key}=${t.state}/${t.s?.id ?? ""}`).join(",");
}

/** What the board's search matches on a Skipper tile. */
export function skipperMatch(v: SkipperView, key: string): string {
  if (key === "watch") return `watcher ${v.watch ?? ""} ${skiffAgentName()}`;
  const t = v.tasks.find((x) => x.key === key);
  return t ? `${t.row.uid} ${t.row.task} ${t.state} ${t.row.project}` : "";
}

/** The first open task's uid for a pane, for its fleet tile. */
export function openUid(v: SkipperView | null, session: string): string | null {
  return v?.tasks.find((t) => t.s?.id === session)?.row.uid ?? null;
}

/** The session a tile goes to: a live task's pane, or Skipper for the watcher tile. */
export function skipperSession(v: SkipperView | null, key: string): string | null {
  if (key === "watch") return skiffAgentSession()?.id ?? null;
  return v?.tasks.find((t) => t.key === key)?.s?.id ?? null;
}

/** The screens to read: each live task's pane and its pre. */
export function skipperScreens(): [string, HTMLPreElement][] {
  return [...screens].filter(([id]) => !!sessions.get(id));
}

// -- Copy

const STATE_WORD: Record<TaskState, string> = {
  gone: "Gone",
  interrupted: "Interrupted",
  blocked: "Blocked",
  pending: "Pending",
  sent: "Running",
};

const WATCH_WORD: Record<WatchTile, string> = {
  stale: "Watcher stale",
  stopped: "Watcher stopped",
  ledger: "Ledger failure",
  undelivered: "Undelivered",
  restarting: "Restarting…",
  "restart-failed": "Restart failed",
};

const clock = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
const short = (id: string | null) => (id ?? "").slice(0, 8);

function watchBody(w: WatchTile): string {
  const name = skiffAgentName();
  const me = skiffAgentSession();
  const f = me?.watch?.failure;
  const deaf = `${name} does not hear when its panes finish.`;
  switch (w) {
    case "stale": {
      const hd = lock?.holder;
      const who = hd ? `pid ${hd.pid}, since ${clock(hd.started_at)}` : "pid unknown";
      return `A watcher holds the lock but did not register with skiffd (${who}). ${deaf}`;
    }
    case "stopped":
      return `No watcher runs. ${deaf}`;
    case "ledger":
      return `The watcher cannot read the ledger: ${f?.message ?? "unknown error"}.`;
    case "undelivered":
      return `At ${clock(f?.at ?? 0)} a watcher line did not start a turn. It can be in ${name}'s input box.`;
    case "restarting":
      return "The watcher restarts. This tile goes when it registers.";
    case "restart-failed":
      return `Restart failed: ${watchRestart.error ?? ""}`;
  }
}

function taskHint(t: OpenTask): string {
  switch (t.state) {
    case "gone":
      return "Reassign it, or mark it resolved.";
    case "interrupted":
      return "The pane restarted. Send it again, or reassign it.";
    case "blocked":
      return "The pane needs you.";
    case "pending":
      return "Not sent yet: the pane was busy.";
    case "sent":
      return `Sent ${t.row.sent_at.replace("T", " ").replace(/:\d\dZ$/, "Z")}`;
  }
}

// -- Drawing

const tone = (st: TaskState | WatchTile) =>
  st === "gone" || st === "stale" || st === "ledger" || st === "restart-failed"
    ? "bad"
    : st === "sent"
      ? "working"
      : st === "pending" || st === "stopped"
        ? "idle"
        : "waiting";
/** A filled square for a state that needs the user, a hollow one for the rest. Color is never the only cue. */
const filled = (st: TaskState | WatchTile) => st === "gone" || st === "blocked" || st === "stale" || st === "ledger" || st === "restart-failed";

function mark(st: TaskState | WatchTile): HTMLElement {
  const m = h("span", `mission-mark ${tone(st)}${filled(st) ? " filled" : ""}`);
  m.setAttribute("aria-hidden", "true");
  return m;
}

/** A foot button: for the mouse. The keys reach the same action through Enter and the menu. */
function footButton(text: string, run: () => void, disabled = false): HTMLButtonElement {
  const b = h("button", "mission-act", text);
  b.type = "button";
  b.tabIndex = -1;
  b.disabled = disabled;
  b.addEventListener("click", (e) => {
    e.stopPropagation();
    run();
  });
  return b;
}

function shell(key: string, cls: string, label: string, lit: boolean): HTMLElement {
  const b = h("div", `mission-tile mission-skipper ${cls}` + (lit ? " lit" : ""));
  b.setAttribute("role", "button");
  b.tabIndex = -1;
  b.dataset.session = key;
  b.setAttribute("aria-label", label);
  b.style.setProperty("--pc", "var(--accent)");
  return b;
}

function watchTileEl(w: WatchTile, lit: boolean): HTMLElement {
  const body = watchBody(w);
  const b = shell("watch", "mission-watch", `Watcher: ${WATCH_WORD[w]}. ${body}`, lit);
  const head = h("span", "mission-head");
  head.append(mark(w), h("span", "mission-state", WATCH_WORD[w]), h("span", "title", "skiff watch"));
  const foot = h("div", "mission-foot");
  foot.append(h("span", "mission-hint"));
  if (w === "undelivered") foot.append(footButton(`Go to ${skiffAgentName()}`, () => watchEnter(w)));
  else foot.append(footButton("Restart watcher", () => watchEnter(w), w === "restarting"));
  foot.append(footButton("Open watch log", openWatchLog));
  b.append(head, h("div", "mission-note", body), foot);
  return b;
}

function taskTileEl(t: OpenTask, lit: boolean): HTMLElement {
  const where = [t.row.project, t.s ? agentName(t.s) : ""].filter(Boolean).join(" · ");
  const b = shell(t.key, "mission-task", "", lit);
  b.dataset.uid = t.row.uid;
  const head = h("span", "mission-head");
  head.append(mark(t.state), h("span", "mission-state", STATE_WORD[t.state]), h("span", "title", t.row.task));
  const meta = h("div", "mission-meta", where || "No project");
  let body: HTMLElement;
  if (t.s) {
    const pre = screens.get(t.s.id) ?? h("pre", "mission-screen task");
    screens.set(t.s.id, pre);
    body = pre;
  } else body = h("div", "mission-note", `Pane ${short(t.row.pane_id)} no longer exists. The task is still open.`);
  const foot = h("div", "mission-foot");
  foot.append(h("span", "mission-uid mono", t.row.uid), h("span", "mission-hint"));
  const main = primary(t);
  if (main) foot.append(footButton(main.label, main.run));
  const more = footButton("…", () => taskMenu(t.key, more));
  more.setAttribute("aria-label", `Task ${t.row.uid} menu`);
  more.classList.add("more");
  foot.append(more);
  b.append(head, meta, body, foot);
  hint(b, t);
  return b;
}

/** The hint line and the label: the state's advice, or what the last action left. */
function hint(b: HTMLElement, t: OpenTask) {
  const note = notes.get(t.row.uid);
  const line = b.querySelector<HTMLElement>(".mission-hint");
  const text = note ?? taskHint(t);
  if (line && line.textContent !== text) line.textContent = text;
  line?.classList.toggle("error", !!note);
  const label = `Task ${t.row.uid}, ${STATE_WORD[t.state]}: ${t.row.task}. ${t.row.project}. ${text}`;
  if (b.getAttribute("aria-label") !== label) b.setAttribute("aria-label", label);
}

function headStatus(v: SkipperView) {
  const me = skiffAgentSession();
  const text = v.watch ? WATCH_WORD[v.watch] : me ? "Watcher runs" : `${skiffAgentName()} does not run`;
  if (status.textContent !== text) status.textContent = text;
  status.className = "mission-watch-state" + (v.watch ? ` ${tone(v.watch)}` : "");
}

/** The lane, first on the board. `lit`: the lit key. */
export function skipperLane(v: SkipperView, lit: string | null): HTMLElement {
  const lane = h("section", "mission-lane skipper");
  const head = h("div", "mission-lane-head");
  const name = h("span", "mission-lane-name", skiffAgentName());
  name.id = "mission-skipper-name";
  lane.setAttribute("aria-labelledby", name.id);
  headStatus(v);
  head.append(name, status, h("span", "mission-open-count", `${v.tasks.length} open`));
  lane.appendChild(head);
  if (v.watch) {
    const g = h("div", "mission-group mission-section-group");
    g.appendChild(watchTileEl(v.watch, lit === "watch"));
    lane.appendChild(g);
  }
  for (const [sec, label] of SECTIONS) {
    const list = v.tasks.filter((t) => sectionOf(t.state) === sec);
    if (!list.length) continue;
    const g = h("div", "mission-group mission-section-group");
    g.appendChild(h("div", "mission-section", label));
    for (const t of list) g.appendChild(taskTileEl(t, lit === t.key));
    lane.appendChild(g);
  }
  const ids = new Set(v.tasks.flatMap((t) => (t.s ? [t.s.id] : [])));
  for (const id of screens.keys()) if (!ids.has(id)) screens.delete(id);
  return lane;
}

/** Everything but the structure, in place: the head, the watcher text, the hints. */
export function freshSkipper(root: HTMLElement, v: SkipperView) {
  headStatus(v);
  const n = root.querySelector(".mission-open-count");
  if (n && n.textContent !== `${v.tasks.length} open`) n.textContent = `${v.tasks.length} open`;
  const note = root.querySelector<HTMLElement>(".mission-watch .mission-note");
  if (v.watch && note && note.textContent !== watchBody(v.watch)) note.textContent = watchBody(v.watch);
  for (const t of v.tasks) {
    const b = root.querySelector<HTMLElement>(`.mission-task[data-session="${CSS.escape(t.key)}"]`);
    if (b) hint(b, t);
  }
}

// -- Actions

const board = () => document.getElementById("mission-control");
/** The keys go back to the board after a menu or a dialog. */
const back = () => board()?.focus();

/** After a ledger change: read again and redraw. The light moves on if its tile left. */
async function after() {
  await read();
  changed();
}

const wrap = (uid: string, f: () => Promise<void>) => async () => {
  notes.delete(uid);
  try {
    await f();
  } catch (e) {
    notes.set(uid, String(e));
  }
  await after();
};

function primary(t: OpenTask): { label: string; run: () => void } | null {
  if (t.state === "gone") return { label: "Reassign…", run: () => reassign(t) };
  if (t.state === "pending" && t.s) return { label: "Send now", run: () => void send(t) };
  if (t.state === "interrupted" && t.s) return { label: "Send again", run: () => void send(t) };
  if (t.s) return { label: "Go to pane", run: () => goToSession(t.s!.id) };
  return null;
}

async function send(t: OpenTask) {
  await wrap(t.row.uid, async () => {
    const blocked = await invoke<string | null>("skiff_ledger_send", { task: t.row.uid });
    if (blocked) notes.set(t.row.uid, blocked);
  })();
}

const markDone = (t: OpenTask) => wrap(t.row.uid, () => invoke("skiff_ledger_update", { task: t.row.uid, state: "done" }))();

/** Mark resolved elsewhere…: the task's pane is gone, its work done somewhere else. */
function resolve(t: OpenTask) {
  promptAction({
    title: `Mark task ${t.row.uid} resolved`,
    body: `Its pane no longer exists. The task becomes done. The result file stays.`,
    placeholder: "Where it was resolved (optional)",
    allowEmpty: true,
    action: "Mark done",
    onClose: back,
    submit: async (ref) => {
      await invoke("skiff_ledger_update", { task: t.row.uid, state: "done", reference: ref || null });
      notes.delete(t.row.uid);
      void after();
    },
  });
}

/** Reassign…: the task goes pending on another live pane, and out at once when that pane is idle. */
function reassign(t: OpenTask) {
  const projectOf = (s: SessionInfo) => place(s)?.project.name ?? "";
  const live = [...sessions.values()]
    .filter((s) => s.state !== "done" && !s.peek && !isSkiffAgent(s.id) && s.id !== t.s?.id)
    .sort((a, b) => {
      const pa = projectOf(a) === t.row.project ? 0 : 1;
      const pb = projectOf(b) === t.row.project ? 0 : 1;
      return pa - pb || projectOf(a).localeCompare(projectOf(b)) || taskTitle(a).localeCompare(taskTitle(b));
    });
  const list = h("div", "mission-pick");
  list.setAttribute("role", "radiogroup");
  list.setAttribute("aria-label", "Panes");
  const sendNow = h("input");
  sendNow.type = "checkbox";
  const sendLabel = h("label", "mission-send");
  const sendNote = h("span", "mission-send-note");
  sendLabel.append(sendNow, h("span", "", "Send it now"), sendNote);
  let chosen: SessionInfo | null = live[0] ?? null;
  const fit = () => {
    const idle = chosen?.state === "idle";
    sendNow.disabled = !idle;
    if (!idle) sendNow.checked = false;
    sendNote.textContent = idle ? "" : "Only to an idle pane.";
  };
  for (const [i, s] of live.entries()) {
    const row = h("label", "mission-pick-row");
    const r = h("input");
    r.type = "radio";
    r.name = "mission-reassign";
    r.checked = i === 0;
    r.addEventListener("change", () => {
      chosen = s;
      fit();
    });
    row.append(r, h("span", "name", `${projectOf(s) || "No project"} · ${taskTitle(s)}`), h("span", "where mono", `${short(s.id)} · ${s.state}`));
    list.appendChild(row);
  }
  if (!live.length) list.appendChild(h("div", "mission-note", "No other pane runs. Start a session first."));
  fit();
  actionDialog({
    title: `Reassign task ${t.row.uid}`,
    body: `The task becomes pending on the pane you pick. ${t.row.task}`,
    content: [list, sendLabel],
    action: "Reassign",
    tone: "go",
    focus: list.querySelector<HTMLElement>("input") ?? undefined,
    onClose: back,
    submit: async () => {
      if (!chosen) throw "Pick a pane.";
      await invoke("skiff_ledger_update", { task: t.row.uid, state: "pending", pane: chosen.id });
      notes.delete(t.row.uid);
      if (sendNow.checked) {
        try {
          const blocked = await invoke<string | null>("skiff_ledger_send", { task: t.row.uid });
          if (blocked) notes.set(t.row.uid, blocked);
        } catch (e) {
          // The reassign stands; the send did not go.
          notes.set(t.row.uid, `Reassigned. Not sent: ${e}`);
        }
      }
      void after();
    },
  });
}

function copyUid(uid: string) {
  navigator.clipboard.writeText(uid).catch(showError);
}

function openResult(path: string) {
  closeMission();
  openPath(path);
}

function watchEnter(w: WatchTile) {
  if (w === "restarting") return;
  if (w === "undelivered") return goToSession(skiffAgentSession()?.id);
  void restartSkiffWatch().then(changed);
}

function findTask(key: string): OpenTask | null {
  return skipperView()?.tasks.find((t) => t.key === key) ?? null;
}

/** Enter or a click on a Skipper tile. */
export function skipperEnter(key: string) {
  if (key === "watch") {
    const w = skipperView()?.watch;
    if (w) watchEnter(w);
    return;
  }
  const t = findTask(key);
  if (!t) return;
  if (enterFor(t.state) === "menu" || !t.s) taskMenu(key);
  else goToSession(t.s.id);
}

/** A pick gives the keys back to the board first, so a dialog it opens returns them there. */
const backFirst = (entries: MenuEntry[]): MenuEntry[] =>
  entries.map((e) => ("run" in e && e.run ? { ...e, run: () => (back(), e.run!()) } : e));

/** The menu key on a Skipper tile: every action the tile has. Bare digits pick an item. */
export function taskMenu(key: string, at?: HTMLElement) {
  const tile = document.querySelector<HTMLElement>(`.mission-skipper[data-session="${CSS.escape(key)}"]`);
  const r = (at ?? tile)?.getBoundingClientRect();
  const x = r ? r.left + 24 : 100;
  const y = r ? r.bottom : 100;
  if (key === "watch") {
    const w = skipperView()?.watch;
    if (!w) return;
    const entries: MenuEntry[] = [
      w === "undelivered"
        ? { label: `Go to ${skiffAgentName()}`, hint: "1", run: () => watchEnter(w) }
        : { label: "Restart watcher", hint: "1", disabled: w === "restarting", run: () => watchEnter(w) },
      { label: "Open watch log", hint: "2", run: openWatchLog },
    ];
    return ctxMenu.open(x, y, "Watcher", backFirst(entries), back);
  }
  const t = findTask(key);
  if (!t) return;
  const live = !!t.s;
  const entries: MenuEntry[] = [];
  if (live) entries.push({ label: "Go to pane", hint: "1", run: () => goToSession(t.s!.id) });
  if (live && (t.state === "pending" || t.state === "interrupted"))
    entries.push({ label: t.state === "pending" ? "Send now" : "Send again", hint: "2", run: () => void send(t) });
  entries.push({ label: "Reassign…", hint: "3", run: () => reassign(t) });
  if (live) entries.push({ label: "Mark done", hint: "4", run: () => void markDone(t) });
  else entries.push({ label: "Mark resolved elsewhere…", hint: "4", run: () => resolve(t) });
  if (t.row.result_path) entries.push({ label: "Open result", hint: "5", run: () => openResult(t.row.result_path!) });
  entries.push({ label: "Copy uid", hint: "6", run: () => copyUid(t.row.uid) });
  ctxMenu.open(x, y, `Task ${t.row.uid}`, backFirst(entries), back);
}
