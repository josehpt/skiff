/**
 * The Skipper lane in Mission control: first on the board whenever Skipper
 * has a folder. A red line at the top while the watcher needs a look, one
 * 20px row per open task in Skipper's ledger, and Skipper's own screen under
 * them. missionControl.ts puts it on the board, draws Skipper's screen and
 * moves the light through it. This file reads the ledger and the watch lock
 * while the board is open, draws the rows, and runs the recovery actions
 * through the `skiff ledger` CLI.
 */

import { invoke } from "@tauri-apps/api/core";

import type { SessionInfo } from "../platform/types";
import { sessions } from "../app/state";
import { place } from "../app/stateQueries";
import {
  isSkiffAgent,
  openSkiffAgent,
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

/** What the watcher line says. Null: the watcher runs well, or its state is not known yet. */
export type WatchTile = "stale" | "stopped" | "ledger" | "undelivered" | "restarting" | "restart-failed";

/** Rows order: what needs a look first, then pending, then running. */
const RANK: Record<TaskState, number> = { gone: 0, interrupted: 0, blocked: 0, pending: 1, sent: 2 };

/** The open state of a ledger row; null for a plan or a done task. */
export function taskState(r: LedgerRow): TaskState | null {
  if (r.kind !== "task" || r.state === "done") return null;
  if (r.interrupted) return "interrupted";
  if (r.live === false) return "gone";
  return r.state === "sent" || r.state === "pending" || r.state === "blocked" ? r.state : null;
}

/**
 * The watcher line for the watcher's state. `held`: a process holds the watch
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

/** Enter on a task row: go to a live task's pane, open a gone task's menu. */
export const enterFor = (st: TaskState): "go" | "menu" => (st === "gone" ? "menu" : "go");

/** The row key of a task. The watcher line's key is `watch`; the line that starts Skipper is `start`. */
const taskKey = (uid: string) => `task:${uid}`;
export const isSkipperKey = (k: string | null | undefined) => !!k && (k === "watch" || k === "start" || k.startsWith("task:"));

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
  /** Skipper's own session; null while it does not run. */
  self: SessionInfo | null;
}

let rows: LedgerRow[] = [];
let lock: { held: boolean; holder: Holder | null } | null = null;
let readAt = 0;
/** The read under way. A second read waits for it, so an action redraws from fresh rows. */
let reading: Promise<void> | null = null;
/** The board's own redraw, after an action changed the ledger. */
let changed: () => void = () => {};

/** How often the open board reads the ledger and the watch lock. */
const READ_EVERY = 3000;

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
    readAt = 0;
  },
};

/** The lane now, or null when Skipper has no folder. */
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
  tasks.sort((a, b) => RANK[a.state] - RANK[b.state]);
  return { watch, tasks, self: me };
}

/** The keys in board order: the watcher line, the task rows, then Skipper's screen or the line that starts it. */
export function skipperKeys(v: SkipperView): string[] {
  return [...(v.watch ? ["watch"] : []), ...v.tasks.map((t) => t.key), v.self?.id ?? "start"];
}

/** The lane's structure: a change rebuilds it. */
export function skipperSignature(v: SkipperView | null): string {
  if (!v) return "";
  return `skipper:${v.watch ?? ""}:${v.self?.id ?? ""}:` + v.tasks.map((t) => `${t.key}=${t.state}/${t.s?.id ?? ""}`).join(",");
}

/** What the board's search matches in the lane. */
export function skipperMatch(v: SkipperView, key: string): string {
  const name = skiffAgentName();
  if (key === "watch") return `watcher ${v.watch ?? ""} ${name}`;
  if (key === "start") return name;
  if (v.self && key === v.self.id) return `${name} ${agentName(v.self)} ${taskTitle(v.self)} ${v.self.state}`;
  const t = v.tasks.find((x) => x.key === key);
  return t ? `${t.row.uid} ${t.row.task} ${t.state} ${STATE_WORD[t.state]} ${t.row.project}` : "";
}

/** The first open task's uid for a pane, for its fleet tile. */
export function openUid(v: SkipperView | null, session: string): string | null {
  return v?.tasks.find((t) => t.s?.id === session)?.row.uid ?? null;
}

/** The session a row goes to: a live task's pane, or Skipper for the watcher line. */
export function skipperSession(v: SkipperView | null, key: string): string | null {
  if (key === "watch") return skiffAgentSession()?.id ?? null;
  return v?.tasks.find((t) => t.key === key)?.s?.id ?? null;
}

// -- Copy

const STATE_WORD: Record<TaskState, string> = {
  gone: "gone",
  interrupted: "restarted",
  blocked: "not sent",
  pending: "pending",
  sent: "running",
};

function watchLine(w: WatchTile): string {
  switch (w) {
    case "stale":
      return "Watcher stale · Enter restarts it";
    case "stopped":
      return "Watcher stopped · Enter starts it";
    case "ledger":
      return "Watcher cannot read the ledger · Enter restarts it";
    case "undelivered":
      return `A watcher line did not start a turn · Enter goes to ${skiffAgentName()}`;
    case "restarting":
      return "Watcher restarts…";
    case "restart-failed":
      return "Watcher restart failed · Enter tries again";
  }
}

/** The watcher's detail, for the line's tooltip and label. */
function watchDetail(w: WatchTile): string {
  const name = skiffAgentName();
  const f = skiffAgentSession()?.watch?.failure;
  switch (w) {
    case "stale": {
      const hd = lock?.holder;
      return `A watcher holds the lock but did not register with skiffd${hd ? ` (pid ${hd.pid})` : ""}. ${name} does not hear when its panes finish.`;
    }
    case "stopped":
      return `No watcher runs. ${name} does not hear when its panes finish.`;
    case "ledger":
      return `The watcher cannot read the ledger: ${f?.message ?? "unknown error"}.`;
    case "undelivered":
      return `A watcher line can be in ${name}'s input box.`;
    case "restarting":
      return "The line goes when the watcher registers.";
    case "restart-failed":
      return watchRestart.error ?? "";
  }
}

/** The first line of a task, less its end stop. The row cuts it to fit. */
const shortTitle = (task: string) => task.split("\n")[0].trim().replace(/\.$/, "");

// -- Drawing

const tone = (st: TaskState) => (st === "gone" ? "bad" : st === "sent" ? "working" : st === "pending" ? "idle" : "waiting");
/** A filled square for a state that needs the user, a hollow one for the rest. Color is never the only cue. */
const filled = (st: TaskState) => st === "gone" || st === "blocked";

function mark(st: TaskState): HTMLElement {
  const m = h("span", `mission-mark ${tone(st)}${filled(st) ? " filled" : ""}`);
  m.setAttribute("aria-hidden", "true");
  return m;
}

/** A 20px row of the lane. It is a tile to the board: it lights, hides on search and takes Enter. */
function row(key: string, cls: string, lit: boolean): HTMLElement {
  const b = h("div", `mission-tile mission-skipper ${cls}` + (lit ? " lit" : ""));
  b.setAttribute("role", "button");
  b.tabIndex = -1;
  b.dataset.session = key;
  b.style.setProperty("--pc", "var(--accent)");
  return b;
}

function watchRow(w: WatchTile, lit: boolean): HTMLElement {
  const b = row("watch", "mission-alert" + (w === "restarting" ? " quiet" : ""), lit);
  b.setAttribute("aria-live", "polite");
  b.setAttribute("aria-label", `${watchLine(w)}. ${watchDetail(w)}`);
  b.title = watchDetail(w);
  b.append(h("span", "title", watchLine(w)));
  return b;
}

function startRow(lit: boolean): HTMLElement {
  const b = row("start", "mission-alert quiet", lit);
  b.append(h("span", "title", `${skiffAgentName()} does not run · Enter starts it`));
  return b;
}

function taskRow(t: OpenTask, lit: boolean): HTMLElement {
  const b = row(t.key, "mission-task", lit);
  b.dataset.uid = t.row.uid;
  const where = [t.row.project, t.s ? agentName(t.s) : ""].filter(Boolean).join(" · ");
  b.title = `${t.row.task}\n${t.row.uid}${where ? ` · ${where}` : ""}`;
  b.setAttribute("aria-label", `Task ${t.row.uid}, ${STATE_WORD[t.state]}: ${t.row.task}. ${where}`);
  b.append(mark(t.state), h("span", "title", shortTitle(t.row.task)), h("span", "mission-state", STATE_WORD[t.state]));
  b.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    e.stopPropagation();
    taskMenu(t.key, undefined, e.clientX, e.clientY);
  });
  return b;
}

/**
 * The lane, first on the board. `lit`: the lit key. `self`: Skipper's screen
 * tile, which the board draws as it draws every session's.
 */
export function skipperLane(v: SkipperView, lit: string | null, self: HTMLElement | null): HTMLElement {
  const lane = h("section", "mission-lane skipper");
  lane.setAttribute("aria-label", skiffAgentName());
  const head = h("div", "mission-lane-head");
  head.append(h("span", "mission-lane-name", skiffAgentName()), h("span", "mission-open-count", `${v.tasks.length} open`));
  lane.appendChild(head);
  if (v.watch) lane.appendChild(watchRow(v.watch, lit === "watch"));
  for (const t of v.tasks) lane.appendChild(taskRow(t, lit === t.key));
  if (v.tasks.length) lane.lastElementChild!.classList.add("last");
  lane.appendChild(self ?? startRow(lit === "start"));
  return lane;
}

/** Everything but the structure, in place: the count and the watcher's detail. */
export function freshSkipper(root: HTMLElement, v: SkipperView) {
  const n = root.querySelector(".mission-open-count");
  if (n && n.textContent !== `${v.tasks.length} open`) n.textContent = `${v.tasks.length} open`;
  const w = root.querySelector<HTMLElement>('.mission-alert[data-session="watch"]');
  if (v.watch && w && w.title !== watchDetail(v.watch)) {
    w.title = watchDetail(v.watch);
    w.setAttribute("aria-label", `${watchLine(v.watch)}. ${watchDetail(v.watch)}`);
  }
}

// -- Actions

const board = () => document.getElementById("mission-control");
/** The keys go back to the board after a menu or a dialog. */
const back = () => board()?.focus();

/** After a ledger change: read again and redraw. The light moves on if its row left. */
async function after() {
  await read();
  changed();
}

/** Runs a ledger action. A failure, or what a send left, shows in the alert bar. */
const wrap = (f: () => Promise<void>) => async () => {
  try {
    await f();
  } catch (e) {
    showError(e);
  }
  await after();
};

async function send(t: OpenTask) {
  await wrap(async () => {
    const blocked = await invoke<string | null>("skiff_ledger_send", { task: t.row.uid });
    if (blocked) showError(`Task ${t.row.uid}: ${blocked}`);
  })();
}

const markDone = (t: OpenTask) => wrap(() => invoke("skiff_ledger_update", { task: t.row.uid, state: "done" }))();

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
      void after();
    },
  });
}

const short = (id: string | null) => (id ?? "").slice(0, 8);

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
      if (sendNow.checked) {
        try {
          const blocked = await invoke<string | null>("skiff_ledger_send", { task: t.row.uid });
          if (blocked) showError(`Task ${t.row.uid}: ${blocked}`);
        } catch (e) {
          // The reassign stands; the send did not go.
          showError(`Reassigned. Not sent: ${e}`);
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

/** Enter or a click on a Skipper row. */
export function skipperEnter(key: string) {
  if (key === "watch") {
    const w = skipperView()?.watch;
    if (w) watchEnter(w);
    return;
  }
  if (key === "start") {
    closeMission();
    return openSkiffAgent();
  }
  const t = findTask(key);
  if (!t) return;
  if (enterFor(t.state) === "menu" || !t.s) taskMenu(key);
  else goToSession(t.s.id);
}

/** A pick gives the keys back to the board first, so a dialog it opens returns them there. */
const backFirst = (entries: MenuEntry[]): MenuEntry[] =>
  entries.map((e) => ("run" in e && e.run ? { ...e, run: () => (back(), e.run!()) } : e));

/**
 * The menu key or a right-click on a Skipper row: the row's other actions.
 * Enter and a click go to the pane. Bare digits pick an item.
 */
export function taskMenu(key: string, at?: HTMLElement, px?: number, py?: number) {
  const tile = document.querySelector<HTMLElement>(`.mission-skipper[data-session="${CSS.escape(key)}"]`);
  const r = (at ?? tile)?.getBoundingClientRect();
  const x = px ?? (r ? r.left + 24 : 100);
  const y = py ?? (r ? r.bottom : 100);
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
  if (live && (t.state === "pending" || t.state === "interrupted"))
    entries.push({ label: t.state === "pending" ? "Send now" : "Send again", hint: "1", run: () => void send(t) });
  entries.push({ label: "Reassign…", hint: "2", run: () => reassign(t) });
  if (live) entries.push({ label: "Mark done", hint: "3", run: () => void markDone(t) });
  else entries.push({ label: "Mark resolved elsewhere…", hint: "3", run: () => resolve(t) });
  if (t.row.result_path) entries.push({ label: "Open result", hint: "4", run: () => openResult(t.row.result_path!) });
  entries.push({ label: "Copy uid", hint: "5", run: () => copyUid(t.row.uid) });
  ctxMenu.open(x, y, `Task ${t.row.uid}`, backFirst(entries), back);
}
