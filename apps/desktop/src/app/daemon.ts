/** skiffd: sessions, projects, events. */
import { placeInSlot } from "../workspace/canvas";
import { filledOf, isSlot } from "../workspace/layoutSlots";
import { agentCallsign } from "../appearance/agentNames";
import { removePane, sessionsOf } from "../workspace/layout";

import type { AgentInfo, Appearance, DaemonEvent, DaemonStatus, Project, Reloaded, SessionInfo, SplitDir } from "../platform/types";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { $, h } from "../ui/dom";
import { rune } from "../ui/runes";
import { showError } from "../ui/alerts";
import { daemonBadge, restartIfIdle } from "./daemonRestart";
import { loadChangesSetting, reloadChanges } from "../workspace/changes";
import { loadProjectThemes } from "../appearance/themes";
import { loadFilesSetting } from "../workspace/files";
import { deleteGroup, loadGroups, syncTemplateName } from "./groups";
import { render, scheduleRender } from "./render";
import { born, gone, OTHER, removeErrors, S, sessions, upsert } from "./state";
import { hasKeys, markSeen, quietSeen } from "./seen";
import { peekExited, peekUpdated, previewToken, strayPeek } from "../terminal/peek";
import { activeGroupObj, awayPlaces, currentWorktree, place, shownIds } from "./stateQueries";
import { agentKind } from "../appearance/agentIcon";
import { loadSkiffAgent } from "./skiffAgent";
import { taskTitle } from "../workspace/model";
import { panes } from "../terminal/terminalState";
import { ptySized } from "../terminal/terminal";
import { reattachPanes } from "../terminal/terminalRuntime";
import { focusPane, inBackground, paneNear, revealSession, rowNear, showSingle, splitWith, unfocus } from "../workspace/view";

export function setDaemon(status: DaemonStatus) {
  const dot = $("daemon").querySelector(".dot") as HTMLElement;
  dot.className = "dot " + (status.connected ? "connected" : "error");
  $("daemon-label").textContent = status.reload
    ? "Reloading skiffd…"
    : status.connected
    ? `skiffd ${status.version ?? ""}${status.replaced ? ` (updated from ${status.replaced})` : status.spawned ? " (started)" : ""}`
    : status.warning?.kind === "hung"
      ? "skiffd not responding"
      : "skiffd unreachable";
  const socket = $("socket");
  if (status.pid) socket.replaceChildren(`${status.socket} · pid ${status.pid}`, pidCopy(status.pid));
  else socket.textContent = status.socket;
  daemonBadge(status.warning);
}

/** Counts event subscriptions. An event from an older one is dropped. */
let eventStream = 0;

/** Streams daemon events to `onEvent`. Replaces an earlier subscription. */
export async function subscribeEvents() {
  const n = ++eventStream;
  const events = new Channel<DaemonEvent>();
  events.onmessage = (e) => {
    if (n === eventStream) onEvent(e);
  };
  await invoke("subscribe_events", { onEvent: events });
}

/**
 * Moves an older skiffd onto the app's own, with its sessions. Rust holds
 * terminal input meanwhile and sends it on once the reload is proven. The
 * page, layout, focus and scroll stay; shown panes redraw from a snapshot.
 */
export async function reloadDaemon(): Promise<DaemonStatus> {
  $("daemon-label").textContent = "Reloading skiffd…";
  // Nothing the reconnect redraws counts as seen.
  quietSeen(true);
  try {
    const r = await invoke<Reloaded>("reload_daemon");
    // Before boot subscribed anything, boot does it next.
    if (r.reconnected && r.status.connected && eventStream) {
      // Events first, so nothing between the list and the stream is missed.
      await subscribeEvents().catch(console.error);
      await refreshSessions().catch(console.error);
      await loadGroups().catch(console.error);
      reattachPanes();
    }
    setDaemon(r.status);
    const end = (t: string) => (t.endsWith(".") ? t : `${t}.`);
    const lines: string[] = [];
    if (r.error) lines.push(end(r.missing.length ? `${r.error}: ${r.missing.join(", ")}` : r.error));
    if (r.lost.length) lines.push(`Input typed during the reload did not reach ${r.lost.join(", ")}.`);
    if (lines.length) showError(lines.join(" "));
    render();
    return r.status;
  } finally {
    quietSeen(false);
  }
}

let copiedTimer: number | undefined;

/** A small button after the pid that copies the number. */
function pidCopy(pid: number): HTMLButtonElement {
  const b = h("button", "status-copy");
  b.type = "button";
  b.title = "Copy pid";
  b.setAttribute("aria-label", `Copy pid ${pid}`);
  b.appendChild(rune("code-copy", 12));
  b.addEventListener("click", () => {
    navigator.clipboard.writeText(String(pid)).then(() => {
      b.classList.add("copied");
      b.title = "Copied";
      clearTimeout(copiedTimer);
      copiedTimer = window.setTimeout(() => {
        b.classList.remove("copied");
        b.title = "Copy pid";
      }, 1200);
    }, showError);
  });
  return b;
}

/** A new session is the single view, unless it came from a pane's Split menu. */
export async function newSession(
  cwd: string | null,
  agent: string | null,
  label: string,
  role: "agent" | "shell",
  where?: { target: string; dir: SplitDir } | { slot: string },
  /** A fixed name, such as the Skiff agent's, in place of a call sign. */
  name?: string,
) {
  // Projects list agents as command lines; the first word is the program.
  const words = (agent ?? "").trim().split(/\s+/).filter(Boolean);
  // The user owns the request until they move: a click or key elsewhere, or a new preview.
  const mark = () => `${previewToken()} ${S.roveKey} ${S.focused} ${S.activeGroup}`;
  const before = mark();
  const visible = S.focused ? panes.get(S.focused) : undefined;
  // Agent IDs are useful in the launch menu, but call signs make a new crew
  // easier (and more fun) to tell apart once it is running.
  const sessionLabel = name ?? (role === "agent" ? agentCallsign([...sessions.values()].map((s) => s.label)) : label);
  const info = await invoke<SessionInfo>("create_session", {
    spec: {
      label: sessionLabel,
      name: role === "agent" ? sessionLabel : null,
      role,
      cwd,
      command: words[0] ?? null,
      args: words.slice(1),
      cols: visible?.term.cols ?? 120,
      rows: visible?.term.rows ?? 40,
    },
  });
  for (const b of born) b.add(info.id);
  upsert(info);
  // The user may have moved to a preview while it started: it shows under the preview.
  const place = () => {
    if (where && "slot" in where) placeInSlot(where.slot, info.id);
    else if (where) splitWith(where.target, where.dir, info.id);
    else showSingle(info.id);
  };
  // Not moved: it shows and takes the keys. Moved: it waits in the list, and a
  // split or empty pane it was asked for still gets it, without the keys.
  if (mark() === before) place();
  else if (where) inBackground(place, true);
  else render();
}

let projectsSeq = 0;

/** The app theme from projects.toml. The `skiff` CLI can change it while the app runs. */
async function loadAppTheme() {
  const a = await invoke<Appearance>("get_appearance").catch(() => null);
  if (a) S.appTheme = a.theme;
}

export async function loadProjects() {
  const seq = ++projectsSeq;
  if (!S.agents.length) S.agents = await invoke<AgentInfo[]>("list_agents").catch(() => []);
  let list: Project[] | null = null;
  let err: string | null = null;
  try {
    [list] = await Promise.all([invoke<Project[]>("list_projects"), loadFilesSetting(), loadChangesSetting(), loadProjectThemes(), loadAppTheme(), loadSkiffAgent()]);
  } catch (e) {
    err = String(e);
  }
  // A later call started while this one waited: its answer is newer.
  if (seq !== projectsSeq) return;
  if (list) {
    const open = list.filter((p) => !p.closed);
    // The same list keeps its objects: the tree and place() compare worktrees by identity.
    if (JSON.stringify(open) !== JSON.stringify(S.projects)) S.projects = open;
    S.closedProjects = list.filter((p) => p.closed);
  }
  S.projectsError = err;
  if (S.selectedProject !== OTHER && !S.projects.some((p) => p.name === S.selectedProject)) {
    const f = S.focused ? sessions.get(S.focused) : undefined;
    S.selectedProject = (f && place(f)?.project.name) || S.projects[0]?.name || null;
  }
  for (const [path] of removeErrors) {
    if (!S.projects.some((p) => p.worktrees.some((w) => w.path === path))) removeErrors.delete(path);
  }
  render();
}

let staleTimer: number | undefined;

/** The worktrees to read changed files for when the burst settles. Null: every known one. */
let stale: Set<string> | null = new Set();

/**
 * Reads the worktrees of the projects that hold `paths` again, and the
 * changed files of `paths`, once a burst of calls settles. Both change
 * outside Skiff: an agent's `git worktree add`, an editor's save. Without
 * `paths`, every project and every known worktree is read.
 */
export function reposStale(paths?: Iterable<string>) {
  if (!paths) stale = null;
  else if (stale) for (const p of paths) stale.add(p);
  window.clearTimeout(staleTimer);
  staleTimer = window.setTimeout(() => {
    const only = stale;
    stale = new Set();
    if (only) void reloadProjectsOf(only);
    else void loadProjects();
    reloadChanges(only ?? undefined);
  }, 400);
}

/**
 * Reads again only the projects that hold `paths`: an agent that stops in
 * one project runs git in that project alone. A project that reads the same
 * keeps its object, so place() keeps its cache.
 */
async function reloadProjectsOf(paths: Set<string>) {
  const names = S.projects.filter((p) => p.worktrees.some((w) => paths.has(w.path))).map((p) => p.name);
  if (!names.length) return;
  const seq = projectsSeq;
  const fresh = await Promise.all(names.map((name) => invoke<Project>("get_project", { name }).catch(() => null)));
  // A full read started meanwhile: its answer is newer.
  if (seq !== projectsSeq) return;
  let next = S.projects;
  for (const p of fresh) {
    const i = p ? next.findIndex((o) => o.name === p.name) : -1;
    if (!p || p.closed || i < 0 || JSON.stringify(next[i]) === JSON.stringify(p)) continue;
    if (next === S.projects) next = [...next];
    next[i] = p;
  }
  if (next === S.projects) return;
  S.projects = next;
  render();
}

/** The worktrees the session works in: its own, and the ones its processes ran in. */
function worktreesOf(s: SessionInfo): string[] {
  const at = place(s);
  return [...(at ? [at.worktree.path] : []), ...awayPlaces(s).map((a) => a.worktree.path)];
}

/** Worktrees with a session that worked while the window was not in front. */
const workedAway = new Set<string>();
let windowAway = !document.hasFocus();

// The app edited projects.toml or a worktree.
void listen("skiff:projects-changed", () => void loadProjects());
window.addEventListener("blur", () => {
  windowAway = true;
  for (const s of sessions.values()) if (s.state === "working") for (const p of worktreesOf(s)) workedAway.add(p);
});
// Back from a terminal, an editor or a diff tool: the worktree in front may have
// changed, and so may the ones agents worked in meanwhile.
window.addEventListener("focus", () => {
  windowAway = false;
  const at = currentWorktree();
  if (at) workedAway.add(at.w.path);
  reposStale(workedAway);
  workedAway.clear();
});
// The focused pane is in front of the user again: its result is seen.
window.addEventListener("focus", () => {
  if (S.focused) scheduleRender();
});

/**
 * What the tree and the headers show for the session: its title and its icon.
 * An agent's leading status glyph is left out, so a spinner frame is not a
 * change. A shell shows its raw title, so a shell keeps every change.
 */
function shownTitle(s: SessionInfo): string {
  const title = taskTitle(s);
  return `${s.role === "shell" ? title : title.replace(/^[^\p{L}\p{N}]+/u, "")}\0${agentKind(s)}`;
}

/** Pulls `last_output_at` and anything missed, so relative times stay true. Drops what the daemon no longer has. */
export async function refreshSessions() {
  const fresh = new Set<string>();
  born.add(fresh);
  let list: SessionInfo[];
  try {
    list = await invoke<SessionInfo[]>("list_sessions");
  } finally {
    born.delete(fresh);
  }
  // Peek commands are not sessions to the user. One that is not the open
  // peek's was left behind, or is an app that runs on.
  for (const s of list) if (s.peek) strayPeek(s);
  list = list.filter((s) => !s.peek);
  const listed = new Set(list.map((s) => s.id));
  for (const s of list) if (!gone.has(s.id)) upsert(s);
  for (const id of [...sessions.keys()]) if (!listed.has(id) && !fresh.has(id)) dropSession(id);
}

export function onEvent(e: DaemonEvent) {
  switch (e.event) {
    case "title": {
      const s = sessions.get(e.session);
      if (!s) return;
      // A spinner frame changes the title string, not what is shown: no render.
      const before = shownTitle(s);
      s.title = e.title;
      if (shownTitle(s) === before) return;
      break;
    }
    case "away": {
      const s = sessions.get(e.session);
      if (s) s.away = e.away;
      break;
    }
    case "state": {
      const s = sessions.get(e.session);
      if (s) {
        s.state = e.state;
        if (e.state === "working") {
          s.last_output_at = Date.now();
          if (windowAway) for (const p of worktreesOf(s)) workedAway.add(p);
        }
        // An agent that stops working may have written files or added a worktree: its worktrees are read again.
        else reposStale(worktreesOf(s));
      }
      restartIfIdle();
      break;
    }
    case "exit": {
      if (peekExited(e.session, e.code)) return;
      const s = sessions.get(e.session);
      if (s) {
        s.state = "done";
        s.exit_code = e.code;
        // A shell that exits (Ctrl+D, `exit`) has nothing left to show: it closes.
        if (s.role === "shell") invoke("kill_session", { session: s.id }).catch(console.error);
      }
      break;
    }
    case "session_created":
      if (gone.has(e.session.id) || e.session.peek) return;
      for (const b of born) b.add(e.session.id);
      upsert(e.session);
      // A restore brings the id back with the size the daemon saved.
      ptySized(e.session);
      break;
    case "session_updated": {
      if (e.session.peek) return peekUpdated(e.session);
      if (!sessions.has(e.session.id)) break;
      upsert(e.session);
      ptySized(e.session);
      // A turn that ends in the pane with the keys leaves nothing to read.
      if (hasKeys(e.session.id)) markSeen(e.session.id);
      break;
    }
    case "session_removed": {
      dropSession(e.session);
      restartIfIdle();
      return;
    }
    case "groups_changed":
      void loadGroups();
      return;
  }
  scheduleRender();
}

/** The daemon pruned the session from its groups; groups_changed brings them. Here the view drops it at once. */
function dropSession(id: string) {
  inBackground(() => dropPane(id));
}

function dropPane(id: string) {
  gone.add(id);
  if (!sessions.has(id)) return;
  // The keys go to a neighbor, picked while the pane and its row are still there:
  // the pane beside it in a group, else the next session row in the list.
  const near = S.focused !== id ? null : shownIds().length > 1 ? paneNear(id) : S.single === id ? rowNear(id) : null;
  sessions.delete(id);
  panes.get(id)?.term.dispose();
  panes.get(id)?.el.remove();
  panes.delete(id);
  S.selection = S.selection.filter((x) => x !== id);
  if (S.focused === id) S.focused = null;
  for (const g of [...S.groups]) {
    if (!sessionsOf(g.layout).includes(id)) continue;
    let next = removePane(g.layout, id);
    if (filledOf(next).length === 0) {
      // The last session ended: the group has nothing left to show, so it goes.
      deleteGroup(g);
      continue;
    }
    if (next) {
      g.layout = next;
      syncTemplateName(g);
    }
    if (g.focus === id) g.focus = filledOf(next)[0] ?? null;
  }
  if (S.single === id) S.single = null;
  const ids = shownIds();
  if (S.focused && !ids.includes(S.focused)) S.focused = null;
  if (near && ids.includes(near)) return focusPane(near);
  if (!ids.length) return near && sessions.has(near) ? revealSession(near) : unfocus();
  if (!S.focused) {
    const next = activeGroupObj()?.focus ?? ids.find((x) => !isSlot(x));
    if (next) return focusPane(next);
  }
  render();
}
