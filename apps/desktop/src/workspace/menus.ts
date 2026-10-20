import { ctxMenu } from "../ui/contextMenu";
/** Right-click menus for projects, sessions, panes and groups. */
import { isSkiffAgent, openWatchLog, skiffAgentFolder, skiffAgentName, skiffWatch, startSkiffWatch } from "../app/skiffAgent";
import { launchIcon } from "../appearance/agentIcon";
import { confirmAction, promptAction } from "../ui/confirm";
import { pickColor } from "../appearance/colorPicker";
import { pickBackgroundOpacity } from "../appearance/backdropOpacity";
import { keyLabel } from "../app/keys";
import { toggleFocusMode, toggleMaximize } from "../app/modes";
import { type MenuEntry } from "../ui/menu";
import { agentName, basename, branchName, byStart, locate, taskTitle } from "./model";
import { cwdOf } from "./canvas";
import { filledOf, slotsOf } from "./layoutSlots";
import type { Group, Project, SessionInfo, SplitDir, Worktree } from "../platform/types";
import type { Fleet } from "./fleets";
import { invoke } from "@tauri-apps/api/core";
import { open as openFile } from "@tauri-apps/plugin-dialog";
import { openPath } from "../platform/os";
import { loadProjects, newSession } from "../app/daemon";
import { launchMenu } from "../app/panels";
import { showError } from "../ui/alerts";
import { hasChanges, reviewChanges, setShowChanges, showsChanges } from "./changes";
import { setShowFiles, showsFiles } from "./files";
import { deleteGroup } from "../app/groups";
import { closeEntries } from "./projectClose";
import { sessionsOf } from "./layout";

import { startRename, startSessionRename } from "./rename";
import { render } from "../app/render";
import { clearSelection, selectionPlan, splitSelection } from "./selection";
import { MAX_PANES, removeErrors, removing, S, selectedWorktree, sessions } from "../app/state";
import { groupOf, launchChoices, launchDigit, place, shownIds, splitFull } from "../app/stateQueries";

import { groupThemeMenu, projectThemeMenu, sessionThemeMenu } from "../appearance/themes";
import { focusPane, refocusTerminal, removeFromGroup, showGroup, splitWith, unsplit } from "./view";
import { resumeAllEntry, resumeOffer, resumePane } from "../terminal/resume";
import { ledgerFolded, ledgerShown, toggleLedger } from "./ledger";

async function setIcon(p: Project, icon: string | null) {
  try {
    await invoke("set_project_icon", { project: p.name, icon });
  } catch (e) {
    showError(e);
  }
}

const DEFAULT_COLOR = "#b69cff";

/** The color picker for a project's accent. */
function pickProjectColor(p: Project) {
  pickColor({
    title: `Color for ${p.name}`,
    start: p.color ?? DEFAULT_COLOR,
    action: "Set color",
    submit: (hex) => invoke("set_project_color", { project: p.name, color: hex }),
    onClose: refocusTerminal,
  });
}

async function setBackground(p: Project, background: string | null) {
  try {
    await invoke("set_project_background", { project: p.name, background });
  } catch (e) {
    showError(e);
  }
}

/** One image behind every pane of the project's terminal area. */
function backgroundEntries(p: Project): MenuEntry[] {
  return [
    {
      icon: "playback-image-plus",
      label: "Choose image…",
      run: async () => {
        const file = await openFile({
          title: `Background for ${p.name}`,
          defaultPath: p.path,
          filters: [{ name: "Images", extensions: ["png", "jpg", "jpeg", "webp", "gif", "svg"] }],
        }).catch(() => null);
        if (typeof file === "string") await setBackground(p, file);
      },
    },
    // Only an image shows through the panes, so the opacity waits for one.
    ...(p.background ? [{ icon: "playback-image", label: "Opacity…", run: () => pickBackgroundOpacity(p) }] : []),
    { icon: "indicators-minus", label: "None", disabled: !p.background, run: () => void setBackground(p, null) },
  ];
}

/** One place for the project icon: pick a file, detect one, or use letters. */
function iconEntries(p: Project): MenuEntry[] {
  return [
    {
      icon: "documents-folder-open",
      label: "Choose file…",
      run: async () => {
        const file = await openFile({
          title: `Icon for ${p.name}`,
          defaultPath: p.path,
          filters: [{ name: "Images", extensions: ["png", "svg", "ico", "jpg", "jpeg", "webp", "gif"] }],
        }).catch(() => null);
        if (typeof file !== "string") return;
        const rel = file.startsWith(p.path + "/") ? file.slice(p.path.length + 1) : file;
        await setIcon(p, rel);
      },
    },
    { icon: "schedule-refresh-cw", label: "Detect", run: () => void setIcon(p, null) },
    { icon: "indicators-minus", label: "Letters only", run: () => void setIcon(p, "") },
  ];
}

/** Ask for the letters on the project's row. An empty field goes back to the first three letters of the name. */
function setInitials(p: Project) {
  promptAction({
    title: `Initials for ${p.name}`,
    body: "Up to 3 characters on the project's row in the side panel. Leave the field empty to use the first 3 letters of the name. A project icon replaces the letters.",
    placeholder: "the initials",
    value: p.short,
    allowEmpty: true,
    maxLength: 3,
    action: "Set initials",
    submit: (short) => invoke("set_project_short", { project: p.name, short }),
    onClose: refocusTerminal,
  });
}

/** The fleets now, for the dialog's note. */
const fleetNames = () => [...new Set(S.projects.map((x) => x.fleet).filter((f): f is string => !!f))];

/** Ask for the fleet the project sits in. Projects with the same fleet go under one fleet head in the tree. */
function setFleet(p: Project) {
  const names = fleetNames();
  promptAction({
    title: `Fleet for ${p.name}`,
    body:
      "Projects with the same fleet name go under one fleet head in the side panel. Leave the field empty to put the project under Projects." +
      (names.length ? ` Fleets now: ${names.join(", ")}.` : ""),
    placeholder: "a fleet name",
    value: p.fleet ?? "",
    allowEmpty: true,
    action: "Set fleet",
    submit: (fleet) => invoke("set_project_fleet", { project: p.name, fleet }),
    onClose: refocusTerminal,
  });
}

/** Give every project of the fleet the new name. An empty name puts each under Projects. */
function renameFleet(f: Fleet, name: string) {
  return invoke<void>("reorder_projects", { order: S.projects.map((x) => x.name), fleets: f.projects.map((p) => [p.name, name]) });
}

/** A right-click on a fleet tile. */
export function fleetMenu(f: Fleet, x: number, y: number) {
  ctxMenu.open(x, y, `Fleet ${f.name}`, [
    {
      icon: "tools-pencil",
      label: "Rename fleet…",
      run: () =>
        promptAction({
          title: `Rename fleet ${f.name}`,
          body: `The ${f.projects.length} projects in it keep the new name.`,
          placeholder: "a fleet name",
          value: f.name,
          action: "Rename",
          submit: (name) => renameFleet(f, name),
          onClose: refocusTerminal,
        }),
    },
    { icon: "code-ungroup", label: "Remove fleet", run: () => void renameFleet(f, "").catch(showError) },
  ]);
}

/** Ask for a branch, then make a worktree for it. Errors stay in the dialog. */
export function newWorktree(p: Project) {
  S.justAdded = null;
  promptAction({
    title: `New worktree in ${p.name}`,
    body: "Git makes a new branch and checks it out in its own folder next to the project.",
    placeholder: "a branch name",
    action: "Create",
    submit: async (branch) => {
      const w = await invoke<Worktree>("add_worktree", { project: p.name, branch, base: null });
      selectedWorktree.set(p.name, w.path);
      await loadProjects();
    },
    onClose: refocusTerminal,
  });
}

/** What each project key does to the selected project. The project menu runs the same. */
export function projectRun(p: Project) {
  return {
    "project-folder": () => void openPath(p.path).catch(showError),
    "project-copy-path": () => void navigator.clipboard.writeText(p.path).catch(showError),
    "project-color": () => pickProjectColor(p),
    "project-changes": () => void setShowChanges(p, !showsChanges(p)),
    "project-files": () => void setShowFiles(p, !showsFiles(p)),
  } as const;
}

/** The project's + in the sidebar, a right-click on its row, or the menu key. */
export function projectMenu(p: Project, x: number, y: number) {
  const run = projectRun(p);
  // New session goes to the project's selected worktree, else its main one, as Ctrl+Shift+T does.
  const sel = selectedWorktree.get(p.name);
  const w = p.worktrees.find((x) => x.path === sel) ?? p.worktrees.find((x) => x.is_main) ?? p.worktrees[0];
  const plus = w && [...document.querySelectorAll<HTMLElement>("#sidebar-scroll .wt-plus")].find((b) => b.dataset.wt === w.path);
  ctxMenu.open(x, y, p.path, [
    { icon: "indicators-plus", label: "New session…", hint: keyLabel("new-session"), disabled: !w, run: () => w && launchMenu.open(plus ?? { x, y }, p, w) },
    { icon: "code-git-branch", label: "New worktree…", hint: keyLabel("new-worktree"), disabled: !!p.error, run: () => newWorktree(p) },
    { icon: "indicators-square-arrow-out-up-right", label: "Show in file manager", hint: keyLabel("project-folder"), run: run["project-folder"] },
    { icon: "code-copy", label: "Copy path", hint: keyLabel("project-copy-path"), run: run["project-copy-path"] },
    { icon: "documents-file-image", label: "Icon…", sub: iconEntries(p) },
    { icon: "tools-pencil", label: "Initials…", run: () => setInitials(p) },
    { icon: "code-group", label: "Fleet…", run: () => setFleet(p) },
    { icon: "tools-sparkles", label: "Color…", hint: keyLabel("project-color"), run: run["project-color"] },
    { icon: "tools-sparkles", label: "Project theme…", hint: keyLabel("theme"), run: () => void projectThemeMenu(p) },
    { icon: "playback-image", label: "Background…", sub: backgroundEntries(p) },
    { icon: "code-git-branch", label: showsChanges(p) ? "Hide changes" : "Show changes", hint: keyLabel("project-changes"), run: run["project-changes"] },
    { icon: "documents-folder-open", label: showsFiles(p) ? "Hide files" : "Show files", hint: keyLabel("project-files"), run: run["project-files"] },
    ...closeEntries(p),
  ]);
}

/** A right-click on a worktree's branch row. */
export function worktreeMenu(p: Project, w: Worktree, x: number, y: number) {
  const plus = [...document.querySelectorAll<HTMLElement>("#sidebar-scroll .wt-plus")].find((b) => b.dataset.wt === w.path);
  const entries: MenuEntry[] = [
    { icon: "indicators-plus", label: "New session…", hint: keyLabel("new-session"), disabled: !!p.error, run: () => launchMenu.open(plus ?? { x, y }, p, w) },
    ...(showsChanges(p) && hasChanges(w) ? [{ icon: "code-git-branch", label: "Review all changes", run: () => reviewChanges(w) }] : []),
    { icon: "indicators-square-arrow-out-up-right", label: "Show in file manager", run: () => void openPath(w.path).catch(showError) },
    { icon: "code-copy", label: "Copy path", run: () => void navigator.clipboard.writeText(w.path).catch(showError) },
  ];
  if (!w.is_main) {
    entries.push({ icon: "tools-trash-2", label: "Remove worktree…", danger: true, disabled: removing.has(w.path), run: () => void removeWorktree(p, w) });
  }
  ctxMenu.open(x, y, branchName(w), entries);
}

function sessionLabel(s: SessionInfo) {
  return `${agentName(s)} · ${taskTitle(s)}`;
}

export function rowMenu(s: SessionInfo, x: number, y: number) {
  if (isSkiffAgent(s.id)) return skiffAgentMenu(s, x, y);
  const ids = S.selection.filter((id) => sessions.has(id));
  if (ids.length > 1 && ids.includes(s.id)) return selectionMenu(ids, x, y);
  ctxMenu.open(x, y, sessionLabel(s), [
    ...(groupOf(s.id)
      ? [{ icon: "code-ungroup", label: "Remove from group", hint: keyLabel("close-pane"), run: () => removeFromGroup(s.id) }]
      : []),
    { icon: "tools-pencil", label: "Rename", hint: keyLabel("rename"), run: () => startSessionRename(s.id) },
    themeEntry(s),
    endEntry(s),
  ]);
}

/**
 * The menu of a row in a multi-selection acts on every selected session. Rename and
 * the theme act on one session, so they are left out. No hints: the keys act on one row.
 */
function selectionMenu(ids: string[], x: number, y: number) {
  const plan = selectionPlan();
  const grouped = ids.filter((id) => groupOf(id));
  ctxMenu.open(x, y, `${ids.length} sessions selected`, [
    ...(plan
      ? [{
          icon: "code-group",
          label: plan.label === "New group" ? `Group ${ids.length} sessions` : plan.label,
          disabled: plan.size > MAX_PANES,
          run: splitSelection,
        }]
      : []),
    ...(grouped.length
      ? [{
          icon: "code-ungroup",
          label: grouped.length === 1 ? "Remove 1 from its group" : `Remove ${grouped.length} from their groups`,
          run: () => {
            clearSelection();
            for (const id of grouped) removeFromGroup(id);
          },
        }]
      : []),
    endSelectedEntry(ids),
  ]);
}

const pick = (m: MenuEntry) => void ("run" in m && m.run?.());

/**
 * Delete or Backspace on a list row. With one way to remove it, that runs
 * (End or removal asks first). With more, a menu at the row asks which.
 */
export function deleteKeyMenu(el: HTMLElement) {
  // A key opened it: show focus rings, so the first item reads as the one picked.
  document.body.classList.add("kbd");
  const r = el.getBoundingClientRect();
  const [x, y] = [r.left + 24, r.bottom];
  const g = S.groups.find((x) => x.id === el.dataset.group);
  if (g && !el.dataset.session) return ctxMenu.open(x, y, `Group: ${g.name}`, groupCloseEntries(g));
  const s = sessions.get(el.dataset.session ?? "");
  if (!s) return;
  const ids = S.selection.filter((id) => sessions.has(id));
  if (ids.length > 1 && ids.includes(s.id)) return pick(endSelectedEntry(ids));
  if (!groupOf(s.id)) return pick(endEntry(s));
  ctxMenu.open(x, y, sessionLabel(s), [
    { icon: "code-ungroup", label: "Remove from group", hint: keyLabel("close-pane"), run: () => removeFromGroup(s.id) },
    endEntry(s),
  ]);
}

function themeEntry(s: SessionInfo): MenuEntry {
  return { icon: "tools-sparkles", label: "Session theme…", hint: keyLabel("theme"), run: () => void sessionThemeMenu(s) };
}

/** Right-click, End session: stops the process and drops it from the list. */
export function endEntry(s: SessionInfo): MenuEntry {
  if (s.state === "done") {
    return {
      icon: "tools-trash-2",
      label: "Remove from list…",
      hint: keyLabel("end-session"),
      danger: true,
      run: async () => {
        const ok = await confirmAction({
          title: `Remove ${taskTitle(s)}?`,
          body: "This session has already exited. Removing it clears it from the session list.",
          action: "Remove session",
        });
        if (ok) void endSession(s.id);
        else refocusTerminal();
      },
    };
  }
  return {
    icon: "indicators-square-stop",
    label: "End session…",
    hint: keyLabel("end-session"),
    danger: true,
    // A running agent loses its work: ask once more.
    run: async () => {
      const ok = await confirmAction({
        title: `End ${taskTitle(s)}?`,
        body: `Stops ${agentName(s)} and removes the session. Its unsaved work in the terminal is lost.`,
        action: "End session",
      });
      if (ok) void endSession(s.id);
      else refocusTerminal();
    },
  };
}

/** Right-click on a multi-selection: ends every selected session, with one confirm. */
function endSelectedEntry(ids: string[]): MenuEntry {
  return endManyEntry(ids, `End ${ids.length} sessions…`, `End ${ids.length} sessions?`);
}

/** Ends several sessions after one confirm. Sessions that already exited go without asking. */
function endManyEntry(ids: string[], label: string, title: string): MenuEntry {
  const running = ids.filter((id) => sessions.get(id)?.state !== "done");
  const end = () => {
    clearSelection();
    render();
    for (const id of ids) void endSession(id);
  };
  if (!running.length) {
    return { icon: "tools-trash-2", label: `Remove ${ids.length} from list`, danger: true, run: end };
  }
  return {
    icon: "indicators-square-stop",
    label,
    danger: true,
    run: async () => {
      const ok = await confirmAction({
        title,
        body: `Stops ${running.length} running ${running.length === 1 ? "process" : "processes"} and removes the sessions. Their unsaved work in the terminal is lost.`,
        action: "End sessions",
      });
      if (ok) end();
      else refocusTerminal();
    },
  };
}

async function endSession(id: string) {
  try {
    await invoke("kill_session", { session: id });
  } catch (e) {
    showError(e);
  }
}

export function paneMenu(id: string, x: number, y: number) {
  const s = sessions.get(id);
  if (!s) return;
  if (id !== S.focused) focusPane(id);
  if (isSkiffAgent(id)) return skiffAgentMenu(s, x, y);
  const full = splitFull();
  const entries: MenuEntry[] = [
    { icon: "layouts-panel-left:flip", label: "Add pane right…", hint: keyLabel("split-right"), disabled: full, run: () => splitMenu(id, "row", x, y) },
    { icon: "layouts-panel-bottom", label: "Add pane below…", hint: keyLabel("split-down"), disabled: full, run: () => splitMenu(id, "col", x, y) },
    { icon: "tools-pencil", label: "Rename", hint: keyLabel("rename"), run: () => startSessionRename(id) },
  ];
  const offer = resumeOffer(s);
  if (offer) entries.unshift({ icon: "schedule-refresh-cw", label: `Resume ${offer.agent}`, run: () => resumePane(id, { x, y }) });
  // These act on the pane you right-clicked, which the line above just focused.
  if (shownIds().length > 1) {
    entries.push({ icon: S.maximized ? "view-restore" : "view-maximize", label: S.maximized ? "Restore pane" : "Maximize pane", hint: keyLabel("maximize"), run: toggleMaximize });
  }
  entries.push({ icon: "view-focus", label: S.focusMode ? "Leave focus mode" : "Focus mode", hint: keyLabel("focus-mode"), run: toggleFocusMode });
  entries.push(themeEntry(s));
  if (shownIds().length > 1) {
    entries.push({ icon: "code-ungroup", label: "Remove from group", hint: keyLabel("close-pane"), run: () => removeFromGroup(id) });
  }
  entries.push(endEntry(s));
  ctxMenu.open(x, y, sessionLabel(s), entries);
}

/**
 * The Skiff agent's row and pane menu: no splits, groups or rename. Its name is in Settings.
 * With no session, the row shows only its folder entries.
 */
export function skiffAgentMenu(s: SessionInfo | null, x: number, y: number) {
  const folder = skiffAgentFolder();
  const entries: MenuEntry[] = folder
    ? [{ icon: "indicators-square-arrow-out-up-right", label: "Show in file manager", run: () => void openPath(folder).catch(showError) }]
    : [];
  if (s) {
    // Only while the row's icon is red: no watcher runs, or its last line failed.
    const watch = skiffWatch(s).state;
    if (watch === "missing") entries.push({ icon: "schedule-refresh-cw", label: "Start watcher", run: startSkiffWatch });
    if (watch === "missing" || watch === "failed") entries.push({ icon: "indicators-square-arrow-out-up-right", label: "Open watch log", run: openWatchLog });
    entries.push(
      { icon: "view-focus", label: S.focusMode ? "Leave focus mode" : "Focus mode", hint: keyLabel("focus-mode"), run: toggleFocusMode },
    );
    if (ledgerShown()) entries.push({ icon: "layouts-panel-left:flip", label: ledgerFolded() ? "Show Ledger" : "Hide Ledger", hint: keyLabel("ledger"), run: toggleLedger });
    entries.push(
      themeEntry(s),
      endEntry(s),
    );
  }
  if (entries.length) ctxMenu.open(x, y, s ? sessionLabel(s) : skiffAgentName(), entries);
}

/** Second step of Split: a new agent in this pane's worktree, or a session not shown. */
export function splitMenu(id: string, dir: SplitDir, x: number, y: number) {
  const s = sessions.get(id);
  if (!s) return;
  const at = place(s);
  const cwd = at ? at.worktree.path : s.cwd;
  const where = at ? branchName(at.worktree) : basename(s.cwd);
  const entries: MenuEntry[] = [{ head: `NEW IN ${where.toUpperCase()}` }];
  launchChoices().forEach((a, i) => {
    entries.push({
      glyph: launchIcon(a),
      label: a?.id ?? "Shell",
      hint: launchDigit(i),
      run: () => void newSession(cwd, a?.command ?? null, a?.id ?? "", a ? "agent" : "shell", { target: id, dir }).catch(showError),
    });
  });
  const shown = shownIds();
  const others = [...sessions.values()].filter((o) => !shown.includes(o.id)).sort(byStart);
  if (others.length) {
    entries.push({ head: "OPEN SESSION" });
    for (const o of others) {
      entries.push({ icon: "·", label: sessionLabel(o), run: () => splitWith(id, dir, o.id) });
    }
  }
  ctxMenu.open(x, y, `${dir === "row" ? "Add pane right" : "Add pane below"}:`, entries);
}

export function groupMenu(g: Group, x: number, y: number) {
  const resume = resumeAllEntry(sessionsOf(g.layout));
  ctxMenu.open(x, y, `Group: ${g.name}`, [
    ...(resume ? [resume] : []),
    ...groupStartEntries(g),
    { head: "GROUP" },
    { icon: "tools-pencil", label: "Rename", hint: keyLabel("rename"), run: () => startRename(g) },
    { icon: "tools-sparkles", label: "Group theme…", hint: keyLabel("theme"), run: () => void groupThemeMenu(g) },
    ...groupCloseEntries(g),
  ]);
}

/** Shell, then agents: a new session in the group's first empty pane, else a new pane on the right. */
function groupStartEntries(g: Group): MenuEntry[] {
  const cwd = cwdOf(g);
  const at = cwd ? locate(S.projects, cwd) : null;
  const where = at ? branchName(at.worktree) : cwd ? basename(cwd) : "home";
  const empty = slotsOf(g.layout)[0];
  const filled = filledOf(g.layout).filter((id) => sessions.has(id));
  const full = !empty && filled.length >= MAX_PANES;
  const entries: MenuEntry[] = [{ head: `NEW IN ${where.toUpperCase()}` }];
  launchChoices().forEach((a, i) => {
    entries.push({
      glyph: launchIcon(a),
      label: a?.id ?? "Shell",
      hint: launchDigit(i),
      disabled: full,
      run: () => {
        if (g.id !== S.activeGroup) showGroup(g.id);
        const target = g.focus && filled.includes(g.focus) ? g.focus : filled[0];
        const place = empty ? { slot: empty } : { target, dir: "row" as const };
        void newSession(cwd, a?.command ?? null, a?.id ?? "", a ? "agent" : "shell", place).catch(showError);
      },
    });
  });
  return entries;
}

/** Drops the group. Its sessions keep running and go back under their worktrees. */
export function ungroup(g: Group) {
  if (g.id === S.activeGroup) unsplit(g);
  else {
    deleteGroup(g);
    render();
  }
}

/** Kill all sessions in the group. It asks first when one runs. The group goes with its last session. */
export async function killGroup(id: string) {
  // Group reloads replace objects while a menu or confirmation is open.
  const group = S.groups.find((x) => x.id === id);
  if (!group) return;
  const ids = sessionsOf(group.layout).filter((s) => sessions.has(s));
  const running = ids.filter((s) => sessions.get(s)?.state !== "done");
  if (running.length) {
    const ok = await confirmAction({
      // Not the group's name: an unnamed group is called "1 over 2", which reads as noise here.
      title: "Kill all sessions in this group?",
      body: `This stops ${running.length} running ${running.length === 1 ? "process" : "processes"} and deletes ${running.length === 1 ? "its session" : "their sessions"}. Unsaved work in ${running.length === 1 ? "it" : "them"} is lost.\nThe group goes when its last session ends.`,
      action: "Kill all sessions",
    });
    if (!ok) return refocusTerminal();
  }
  clearSelection();
  const results = await Promise.allSettled(ids.filter((id) => sessions.has(id)).map((id) => invoke("kill_session", { session: id })));
  for (const result of results) {
    if (result.status === "rejected") {
      showError(result.reason);
    }
  }
  render();
}

/** The same group actions serve context menus, the menu bar, and the palette. */
export function groupCloseEntries(g: Group | null): Exclude<MenuEntry, { head: string }>[] {
  const current = () => S.groups.find((x) => x.id === g?.id);
  const idsOf = (group: Group) => sessionsOf(group.layout).filter((id) => sessions.has(id));
  const hasSessions = !!g && idsOf(g).length > 0;
  const drop = () => {
    // Group reloads replace objects while a menu or confirmation is open.
    const group = current();
    if (group) ungroup(group);
  };
  const entries: Exclude<MenuEntry, { head: string }>[] = [
    { icon: "code-ungroup", label: "Ungroup", hint: keyLabel("close-pane"), disabled: !g, run: drop },
  ];
  if (hasSessions) entries.push(
    { icon: "indicators-square-stop", label: "Kill all sessions…", hint: keyLabel("end-session"), danger: true, run: () => void killGroup(g!.id) },
  );
  return entries;
}

export async function removeWorktree(p: Project, w: Worktree) {
  if (removing.has(w.path)) return;
  const ok = await confirmAction({
    title: `Remove worktree ${branchName(w)}?`,
    body: `Deletes the folder ${w.path}.\nThe branch stays. Git also deletes ignored files in it, such as .env and build output.`,
    action: "Remove worktree",
  });
  if (!ok) return refocusTerminal();
  removing.add(w.path);
  removeErrors.delete(w.path);
  render();
  try {
    try {
      await invoke("remove_worktree", { project: p.name, path: w.path, force: false });
    } catch (e) {
      // skiffd refuses uncommitted changes. Ask again, and force only on a yes.
      if (!String(e).includes("uncommitted changes")) throw e;
      const force = await confirmAction({
        title: `Worktree ${branchName(w)} has uncommitted changes`,
        body: "Removing it deletes them. You cannot get them back.\nCommits on the branch stay.",
        action: "Remove anyway",
      });
      if (!force) return refocusTerminal();
      await invoke("remove_worktree", { project: p.name, path: w.path, force: true });
    }
    if (selectedWorktree.get(p.name) === w.path) selectedWorktree.delete(p.name);
    await loadProjects();
  } catch (e) {
    removeErrors.set(w.path, String(e));
  } finally {
    removing.delete(w.path);
    render();
  }
}
