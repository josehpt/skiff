/**
 * Close and remove a project. Close keeps it in projects.toml with its
 * settings, out of the tree; Remove drops it. Both end its sessions and leave
 * the folder on disk.
 */
import { projectOrder } from "./fleets";
import { confirmAction } from "../ui/confirm";
import type { MenuEntry } from "../ui/menu";
import { ctxMenu } from "../ui/contextMenu";
import { agentName, branchName, taskTitle } from "./model";
import type { Project } from "../platform/types";
import { invoke } from "@tauri-apps/api/core";
import { loadProjects } from "../app/daemon";
import { showError } from "../ui/alerts";
import { render } from "../app/render";
import { clearSelection } from "./selection";
import { collapsed, S, selectedWorktree, sessions } from "../app/state";
import { worktreeSessions } from "../app/stateQueries";

import { refocusTerminal, selectProject } from "./view";
import { hostInvoke } from "../app/hostCaps";

/** The project's sessions, and one line per worktree that has any, for a confirmation. */
function projectSessions(p: Project) {
  const ids: string[] = [];
  const lines: string[] = [];
  for (const w of p.worktrees) {
    const list = worktreeSessions(w);
    if (!list.length) continue;
    lines.push(`${branchName(w)}:`);
    for (const s of list) {
      ids.push(s.id);
      const mark = s.state === "working" ? " (working)" : s.state === "waiting" ? " (waiting)" : "";
      lines.push(`  ${agentName(s)} · ${taskTitle(s)}${mark}`);
    }
  }
  const running = ids.filter((id) => sessions.get(id)?.state !== "done");
  return { ids, running, list: lines.join("\n") };
}

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;

/** Ends the sessions. False when one would not end: the project stays as it was. */
async function endAll(ids: string[]): Promise<boolean> {
  clearSelection();
  const results = await Promise.allSettled(ids.filter((id) => sessions.has(id)).map((id) => hostInvoke("session_lifecycle", "kill_session", { session: id })));
  let ok = true;
  for (const r of results) {
    if (r.status === "rejected") {
      ok = false;
      showError(r.reason);
    }
  }
  return ok;
}

/** The project next to `p` in project order, else the one before it. */
function neighbor(p: Project): Project | null {
  const all = projectOrder();
  const i = all.findIndex((x) => x.name === p.name);
  return all[i + 1] ?? all[i - 1] ?? null;
}

/** After a close or remove: the tree loses the project, and a neighbor shows. */
async function leave(p: Project) {
  const next = S.selectedProject === p.name ? neighbor(p) : null;
  if (S.justAdded === p.name) S.justAdded = null;
  await loadProjects();
  if (next) selectProject(next.name);
  else render();
  refocusTerminal();
}

/** Closes the project. With running sessions, asks first: they end. */
export async function closeProject(p: Project) {
  const { ids, running, list } = projectSessions(p);
  if (running.length) {
    const ok = await confirmAction({
      title: `Close ${p.name}?`,
      body: `Ends ${plural(running.length, "running session")}. Their unsaved work in the terminal is lost.\n\n${list}\n\nThe project keeps its settings. Open it again from File › Open closed project.`,
      action: `End ${plural(running.length, "session")} and close`,
    });
    if (!ok) return refocusTerminal();
  }
  if (!(await endAll(ids))) return;
  try {
    await invoke("set_project_closed", { project: p.name, closed: true });
  } catch (e) {
    return showError(e);
  }
  await leave(p);
}

/** Removes the project and its settings, open or closed, after a confirmation. */
export async function removeProject(p: Project) {
  const { ids, running, list } = projectSessions(p);
  const ends = running.length
    ? `\n\nEnds ${plural(running.length, "running session")}. Their unsaved work in the terminal is lost.\n\n${list}`
    : "";
  const ok = await confirmAction({
    title: `Remove ${p.name}?`,
    body: `Skiff forgets this project and its settings: color, icon, background and theme. The folder ${p.path} and its worktrees stay on disk.${ends}`,
    action: running.length ? `End ${plural(running.length, "session")} and remove` : "Remove project",
  });
  if (!ok) return refocusTerminal();
  if (!(await endAll(ids))) return;
  try {
    await invoke("remove_project", { project: p.name });
  } catch (e) {
    return showError(e);
  }
  for (const w of p.worktrees) collapsed.delete(w.path);
  selectedWorktree.delete(p.name);
  await leave(p);
}

/** Opens a closed project again, with its settings, and shows it. */
export async function openClosedProject(p: Project) {
  try {
    await invoke("set_project_closed", { project: p.name, closed: false });
  } catch (e) {
    return showError(e);
  }
  await loadProjects();
  selectProject(p.name);
}

/** The project menu's last entries. */
export function closeEntries(p: Project): MenuEntry[] {
  return [
    { icon: "indicators-minus", label: "Close project", run: () => void closeProject(p) },
    { icon: "tools-trash-2", label: "Remove project…", danger: true, run: () => void removeProject(p) },
  ];
}

/** File › Open closed project: each closed project, to open or remove. */
export function closedProjectsMenu(x: number, y: number) {
  const entries: MenuEntry[] = S.closedProjects.map((p) => ({
    icon: "documents-folder-open",
    label: p.name,
    sub: [
      { icon: "documents-folder-open", label: "Open", run: () => void openClosedProject(p) },
      { icon: "tools-trash-2", label: "Remove…", danger: true, run: () => void removeProject(p) },
    ],
  }));
  if (!entries.length) entries.push({ label: "No closed projects", disabled: true });
  ctxMenu.open(x, y, "Closed projects", entries);
}
