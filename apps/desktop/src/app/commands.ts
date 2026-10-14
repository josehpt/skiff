import { runAction, openProjectMenu } from "./actions";
import { showAbout } from "../ui/infoDialogs";
import { isSkiffAgent, skiffAgentFolder, skiffAgentName, skiffAgentSession } from "./skiffAgent";

/** One command list for the menu bar and the palette; key actions. */
import { type Action, keyLabel, menuAccel } from "./keys";
import { agentName, branchName, bySessionPriority, taskTitle } from "../workspace/model";
import { createSwitcher, type SwitchItem } from "../ui/switcher";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "../platform/os";

import { showError, showNotice } from "../ui/alerts";
import { confirmAction } from "../ui/confirm";

import { copyReport, hasReport, startTrace, stopTrace, tracing } from "../diagnostics/latency";
import { groupCloseEntries } from "../workspace/menus";

import { closedProjectsMenu, closeProject, openClosedProject, removeProject } from "../workspace/projectClose";

import { scheduleRender } from "./render";
import { S, sessions } from "./state";
import { accent, activeGroupObj, currentProject, currentWorktree, inShownGroup, place, shownIds, splitFull, worktreeSessions } from "./stateQueries";

import { sessionThemeMenu } from "../appearance/themes";
import { modeKeyBlocked } from "./modes";
import { refocusTerminal, revealSession, selectWorktree } from "../workspace/view";

interface Cmd {
  section: "File" | "Edit" | "View" | "Help";
  label: string;
  key: string;
  /** The keymap action, so the menu bar can show and bind its key. */
  action?: Action;
  run: () => void;
  /** Shown greyed out in the menu, left out of the palette. */
  off?: boolean;
}

function commands(): Cmd[] {
  const s = S.focused ? sessions.get(S.focused) : undefined;
  const shown = !!(S.focused && shownIds().includes(S.focused));
  const g = activeGroupObj();
  const k = (a: Action) => keyLabel(a);
  const act = (a: Action) => () => runAction(a);
  return [
    { section: "File", label: "New session…", key: k("new-session"), action: "new-session", run: act("new-session"), off: !currentWorktree() },
    { section: "File", label: "New worktree…", key: k("new-worktree"), action: "new-worktree", run: act("new-worktree"), off: !currentProject() },
    { section: "File", label: "Add project…", key: k("add-project"), action: "add-project", run: act("add-project") },
    { section: "File", label: "Open closed project…", key: "", run: openClosedMenu, off: !S.closedProjects.length },
    { section: "File", label: "Project menu…", key: "", run: openProjectMenu, off: !currentProject() },
    { section: "File", label: "Close project", key: "", run: () => withProject(closeProject), off: !currentProject() },
    { section: "File", label: "Remove project…", key: "", run: () => withProject(removeProject), off: !currentProject() },
    { section: "File", label: "Settings", key: k("settings"), action: "settings", run: act("settings") },
    { section: "File", label: "Quit", key: k("quit"), action: "quit", run: act("quit") },
    { section: "Edit", label: "Copy", key: k("copy"), action: "copy", run: act("copy"), off: !s },
    { section: "Edit", label: "Paste", key: k("paste"), action: "paste", run: act("paste"), off: !s },
    { section: "Edit", label: "Find…", key: k("find"), action: "find", run: act("find"), off: !shown },
    { section: "Edit", label: "Rename session", key: k("rename"), action: "rename", run: act("rename"), off: !s || isSkiffAgent(s.id) },
    { section: "Edit", label: "Session theme…", key: k("theme"), run: () => s && void sessionThemeMenu(s), off: !s },
    {
      section: "Edit", label: "End session…", key: k("end-session"), action: "end-session", off: !s,
      run: act("end-session"),
    },
    { section: "View", label: "Command palette", key: k("palette"), action: "palette", run: act("palette") },
    { section: "View", label: "Next session", key: k("session-next"), action: "session-next", run: act("session-next") },
    { section: "View", label: "Previous session", key: k("session-prev"), action: "session-prev", run: act("session-prev") },
    { section: "View", label: "Main worktree", key: k("list-main"), action: "list-main", run: act("list-main") },
    { section: "View", label: "Add pane right…", key: k("split-right"), action: "split-right", run: act("split-right"), off: !shown || splitFull() || isSkiffAgent(S.focused) },
    { section: "View", label: "Add pane below…", key: k("split-down"), action: "split-down", run: act("split-down"), off: !shown || splitFull() || isSkiffAgent(S.focused) },
    { section: "View", label: "Remove from group", key: k("close-pane"), action: "close-pane", run: act("close-pane"), off: !(S.focused && inShownGroup(S.focused)) },
    ...groupCloseEntries(g).map((entry): Cmd => ({
      section: "View", label: entry.label, key: "", run: () => entry.run?.(), off: entry.disabled,
    })),
    { section: "View", label: "Mission control", key: k("mission-control"), action: "mission-control", run: act("mission-control") },
    { section: "View", label: "Next waiting", key: k("next-waiting"), action: "next-waiting", run: act("next-waiting") },
    {
      section: "View", label: `${skiffAgentSession() ? "Show" : "Start"} ${skiffAgentName()}`, key: k("skiff-agent"), action: "skiff-agent",
      run: act("skiff-agent"), off: !skiffAgentFolder(),
    },
    { section: "View", label: "Back to last session", key: k("back"), action: "back", run: act("back"), off: !S.previous },
    { section: "View", label: S.focusMode ? "Leave focus mode" : "Focus mode", key: k("focus-mode"), action: "focus-mode", run: act("focus-mode"), off: (!S.focusMode && !shown) || modeKeyBlocked("focus-mode") },
    { section: "View", label: S.maximized ? "Restore pane" : "Maximize pane", key: k("maximize"), action: "maximize", run: act("maximize"), off: (!S.maximized && !shown) || modeKeyBlocked("maximize") },
    { section: "View", label: "Bigger text", key: k("font-bigger"), action: "font-bigger", run: act("font-bigger") },
    { section: "View", label: "Smaller text", key: k("font-smaller"), action: "font-smaller", run: act("font-smaller") },
    { section: "View", label: "Reset text size", key: k("font-reset"), action: "font-reset", run: act("font-reset") },
    { section: "View", label: "Bigger app text", key: k("app-font-bigger"), action: "app-font-bigger", run: act("app-font-bigger") },
    { section: "View", label: "Smaller app text", key: k("app-font-smaller"), action: "app-font-smaller", run: act("app-font-smaller") },
    { section: "View", label: "Reset app text size", key: k("app-font-reset"), action: "app-font-reset", run: act("app-font-reset") },
    { section: "Help", label: "Keyboard shortcuts", key: k("shortcuts"), action: "shortcuts", run: act("shortcuts") },
    { section: "Help", label: "Open projects.toml", key: "", run: () => void openConfig() },
    { section: "Help", label: "Install skiff command…", key: "", run: () => void installCli() },
    {
      section: "Help", label: tracing() ? "Stop latency trace" : "Start latency trace", key: "",
      run: () => {
        tracing() ? stopTrace() : startTrace();
        scheduleRender();
      },
    },
    { section: "Help", label: "Copy latency report", key: "", run: () => void copyReport(), off: !hasReport() },
    { section: "Help", label: "Report an issue", key: "", run: () => void openUrl(ISSUES_URL).catch(showError) },
    { section: "Help", label: "About Skiff", key: "", run: () => void showAbout() },
  ];
}

const ISSUES_URL = "https://github.com/solutionscay/skiff/issues/new";

/** Where a group of commands starts a new block in its menu. */
const BREAK_BEFORE = new Set(["Project menu…", "Settings", "Copy", "Rename session", "Command palette", "Add pane right…", "Next waiting", "Focus mode", "Leave focus mode", "Bigger text", "Open projects.toml", "About Skiff"]);

let menuRuns = new Map<string, () => void>();
let menuSent = "";

/** Sends the command list to the native menu bar when it changes. */
export function syncMenu() {
  const menus: { title: string; items: { id?: string; label?: string; accel?: string; enabled?: boolean }[] }[] = [];
  const runs = new Map<string, () => void>();
  for (const [i, c] of commands().entries()) {
    let m = menus.at(-1);
    if (m?.title !== c.section) menus.push((m = { title: c.section, items: [] }));
    else if (BREAK_BEFORE.has(c.label)) m.items.push({});
    const id = `cmd:${i}`;
    runs.set(id, c.run);
    m.items.push({ id, label: c.label, accel: c.action ? menuAccel(c.action) : "", enabled: !c.off });
  }
  menuRuns = runs;
  const json = JSON.stringify(menus);
  if (json === menuSent) return;
  menuSent = json;
  invoke("set_menu", { menus }).catch(console.error);
}

void listen<string>("skiff:menu", (e) => menuRuns.get(e.payload)?.());
// The search button in the GNOME header bar.
void listen("skiff:search", () => runAction("palette"));

async function installCli() {
  const ok = await confirmAction({
    title: "Install skiff command",
    body: "Put the skiff command on your PATH, so shells and agents can drive Skiff. macOS can ask for your password.",
    action: "Install",
  });
  if (!ok) return;
  try {
    const path = await invoke<string>("install_cli");
    showNotice(`The skiff command is at ${path}. Open a new shell to use it.`);
  } catch (e) {
    if (String(e) !== "cancelled") showError(e);
  }
}

async function openConfig() {
  await invoke("open_config").catch(showError);
}

/** The palette lists every enabled command, then sessions and worktrees. */
function paletteCommands(): SwitchItem[] {
  return commands()
    .filter((c) => !c.off && c.label !== "Command palette")
    .map((c) => ({
      kind: "command" as const,
      color: "var(--text-3)",
      primary: c.label,
      secondary: c.section,
      meta: c.key,
      text: `${c.section} ${c.label}`,
      run: c.run,
    }));
}

export const switcher = createSwitcher(() => {
  const items: SwitchItem[] = paletteCommands();
  const found: SwitchItem[] = [...sessions.values()].sort(bySessionPriority).map((s) => {
    const at = place(s);
    const where = at ? `${at.project.name} / ${branchName(at.worktree)}` : `other / ${s.cwd}`;
    return {
      kind: "session",
      color: accent(at?.project),
      primary: `${agentName(s)} · ${taskTitle(s)}`,
      secondary: where,
      state: s.state,
      meta: s.state === "waiting" || s.state === "idle" ? s.state : "",
      text: `${where} ${agentName(s)} ${taskTitle(s)} ${s.state}`,
      run: () => revealSession(s.id),
    };
  });
  items.push(...found);
  for (const p of S.closedProjects) {
    items.push({
      kind: "command",
      color: accent(p),
      primary: `Open project: ${p.name}`,
      secondary: "closed",
      meta: "",
      text: `open closed project ${p.name}`,
      run: () => void openClosedProject(p),
    });
  }
  for (const p of S.projects) {
    for (const w of p.worktrees) {
      items.push({
        kind: "worktree",
        color: accent(p),
        primary: branchName(w),
        secondary: p.name,
        meta: `${worktreeSessions(w).length} sessions`,
        text: `${p.name} / ${branchName(w)} worktree`,
        run: () => selectWorktree(p, w),
      });
    }
  }
  return items;
}, refocusTerminal);

const withProject = (f: (p: NonNullable<ReturnType<typeof currentProject>>) => Promise<void>) => {
  const p = currentProject();
  if (p) void f(p);
};

/** The closed projects, under the tree's Projects head. */
function openClosedMenu() {
  const r = document.querySelector<HTMLElement>("#sidebar-scroll .tree-head:not(.fleet-head)")?.getBoundingClientRect();
  closedProjectsMenu(r ? r.left + 24 : 80, r ? r.bottom : 80);
}
