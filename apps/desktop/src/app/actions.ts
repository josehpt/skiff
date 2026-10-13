import { type Action } from "./keys";

import { invoke } from "@tauri-apps/api/core";

import { $ } from "../ui/dom";
import { copy, open, reveal } from "../platform/fileActions";
import { showError } from "../ui/alerts";
import { currentRow, cycleRegion, fromProject, listItems, stepList, stepRail, toMainWorktree, toProject } from "./keyboard";

import { endEntry, killGroup, newWorktree, projectMenu, projectRun, splitMenu, ungroup } from "../workspace/menus";
import { ctxMenu } from "../ui/contextMenu";
import { addProject, launchMenu, settings } from "./panels";

import { renameListItem, startRename, startSessionRename } from "../workspace/rename";

import { FONT_DEFAULT, S, sessions } from "./state";
import { currentProject, currentWorktree, shownIds, splitFull } from "./stateQueries";

import { paneCenter } from "../terminal/terminal";

import { openFind } from "../terminal/terminalSearch";
import { copySelection, pasteClipboard } from "../terminal/terminalClipboard";
import { fontTarget, setFontSize, stepPaneFont } from "../terminal/terminalFont";

import { modeGate, modeKeyBlocked, toggleFocusMode, toggleMaximize } from "./modes";
import { closePane, focusNextWaiting, goBack, moveFocus, removeFromGroup } from "../workspace/view";
import { switcher } from "./commandUi";
import { maximizeLit, missionOpen, toggleMission } from "../workspace/missionControl";
import { showShortcuts } from "../ui/infoDialogs";
import { groupThemeMenu, projectThemeMenu, sessionThemeMenu } from "../appearance/themes";
function openNewSession() {
  // Its key works from inside a menu, such as the worktree menu that shows it: that menu goes.
  ctxMenu.close(false);
  // The highlighted row's worktree, as its + would; else the focused one.
  const row = S.atRail ? undefined : currentRow();
  const rowPlus = row?.closest("#sidebar-scroll .wt")?.querySelector<HTMLElement>(".wt-plus");
  if (rowPlus) return rowPlus.click();
  const at = currentWorktree();
  if (!at) return;
  const plus = [...document.querySelectorAll<HTMLElement>("#sidebar-scroll .wt-plus")].find((b) => b.dataset.wt === at.w.path);
  launchMenu.open(plus ?? $("rail"), at.p, at.w);
}

export function runAction(a: Action) {
  const shown = S.focused && shownIds().includes(S.focused);
  // Mission control holds the keys as a dialog does. Its own key closes it; maximize opens the lit session.
  if (missionOpen() && a === "maximize") return maximizeLit();
  if (missionOpen() && a !== "mission-control" && a !== "quit") return;
  // The native menu runs its accelerators through here, not through the key handler.
  if (modeKeyBlocked(a) || modeGate(a)) return;
  // The open palette holds every other action; its own key closes it.
  if (switcher.isOpen() && a !== "palette") return;
  // Settings holds the keys as a dialog does. The list keys step its sections.
  if (settings.isOpen && (settings.runKey(a) || (a !== "settings" && a !== "quit"))) return;
  switch (a) {
    case "palette": return switcher.toggle();
    case "new-session": return openNewSession();
    case "new-worktree": {
      const p = currentProject();
      return p ? newWorktree(p) : undefined;
    }
    case "add-project": return void addProject.open();
    case "project-menu": {
      // The highlighted row's menu, as a right-click opens it: a worktree, a
      // group, a session, or a Changes or Files row. The
      // highlight, not DOM focus: a picked group can be current while focus
      // rests elsewhere. On the rail, or with no row, the project menu.
      const row = S.atRail || document.activeElement?.closest("#rail") ? undefined : currentRow();
      if (!row) return openProjectMenu();
      const b = row.getBoundingClientRect();
      row.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: b.left + 24, clientY: b.bottom }));
      return;
    }
    case "project-folder":
    case "project-copy-path": {
      // On a file or change row, the key acts on that file.
      const path = rowPath();
      if (path) return a === "project-copy-path" ? copy(path.path) : path.dir ? open(path.path) : reveal(path.path);
      const p = currentProject();
      return p ? projectRun(p)[a]() : undefined;
    }
    case "project-color":
    case "project-changes":
    case "project-files": {
      const p = currentProject();
      return p ? projectRun(p)[a]() : undefined;
    }
    case "split-right":
    case "split-down": {
      if (!S.focused || !shown || splitFull()) return;
      const [x, y] = paneCenter();
      return splitMenu(S.focused, a === "split-right" ? "row" : "col", x, y);
    }
    case "close-pane": return closeKey(!!shown);
    case "next-waiting": return focusNextWaiting();
    case "mission-control": return toggleMission();
    case "back": return goBack();
    case "rename": {
      // In the list, the row with the keys: arrowing to a row does not open it at once.
      if (renameListItem(document.activeElement as HTMLElement | null)) return;
      const pg = S.groups.find((x) => x.id === S.groupPicked);
      return pg ? startRename(pg) : S.focused ? startSessionRename(S.focused) : undefined;
    }
    case "theme": return openTheme();
    case "end-session": return endCurrentSession();
    case "copy": return void copySelection();
    case "paste": return void pasteClipboard();
    case "find": return openFind();
    case "font-bigger": return fontKey(1);
    case "font-smaller": return fontKey(-1);
    case "font-reset": return fontKey(0);
    case "app-font-bigger": return setFontSize(S.fontSize + 1);
    case "app-font-smaller": return setFontSize(S.fontSize - 1);
    case "app-font-reset": return setFontSize(FONT_DEFAULT);
    case "settings": return void settings.open();
    case "quit": return void invoke("quit_app").catch(showError);
    case "focus-left": return moveFocus("left");
    case "focus-right": return moveFocus("right");
    case "focus-up": return moveFocus("up");
    case "focus-down": return moveFocus("down");
    case "region-next": return cycleRegion(1);
    case "region-prev": return cycleRegion(-1);
    case "session-next": return S.atRail ? stepRail(1) : stepList(1);
    case "session-prev": return S.atRail ? stepRail(-1) : stepList(-1);
    case "list-project": return toProject();
    case "list-back": return fromProject();
    case "list-main": return toMainWorktree();
    case "shortcuts": return showShortcuts();
    case "focus-mode": return toggleFocusMode();
    case "maximize": return toggleMaximize();
  }
}

/**
 * The close key picks its target as Rename does. A session row leaves its
 * group, and a group row ungroups. Else the focused pane leaves its group;
 * a single pane has none, so nothing happens. Sessions keep running in every case.
 */
function closeKey(shown: boolean) {
  const el = document.activeElement as HTMLElement | null;
  const row = el && listItems().includes(el) ? el : undefined;
  if (row?.dataset.session) return removeFromGroup(row.dataset.session);
  const gid = row ? row.dataset.group : S.groupPicked;
  const g = gid ? S.groups.find((x) => x.id === gid) : undefined;
  if (g) return ungroup(g);
  if (!row && shown) closePane(S.focused!);
}

/**
 * The theme key acts on the highlighted row, as the menu key does: a session
 * row gets its own theme and a group row every terminal in it. On the rail, or
 * on a project, worktree, Changes or Files row, the project theme. With no row,
 * the focused terminal.
 */
function openTheme() {
  const project = () => {
    const p = currentProject();
    if (p) void projectThemeMenu(p);
  };
  if (S.atRail || document.activeElement?.closest("#rail")) return project();
  const row = currentRow();
  const sid = row ? row.dataset.session : S.focused;
  const s = sid ? sessions.get(sid) : undefined;
  if (s) return void sessionThemeMenu(s);
  const g = row?.dataset.group ? S.groups.find((x) => x.id === row.dataset.group) : undefined;
  if (g) return void groupThemeMenu(g);
  project();
}

/** End the highlighted session, or the focused pane's session, as its menu item does. */
function endCurrentSession() {
  const row = S.atRail ? undefined : currentRow();
  // On a group row the key acts on the group: all its sessions.
  if (row?.dataset.group) return void killGroup(row.dataset.group);
  const session = sessions.get(row?.dataset.session ?? S.focused ?? "");
  const entry = session && endEntry(session);
  if (entry && "run" in entry) entry.run?.();
}

/** The selected project's menu, at its rail icon. */
export function openProjectMenu() {
  const p = currentProject();
  const chip = document.querySelector<HTMLElement>("#rail .rail-chip.active");
  if (!p || !chip) return;
  const r = chip.getBoundingClientRect();
  projectMenu(p, r.right, r.top);
}

/** The file of the highlighted Files or Changes row, if that is the row. */
function rowPath(): { path: string; dir: boolean } | null {
  const row = S.atRail ? undefined : currentRow();
  const m = row?.dataset.key?.match(/^(file|change):(.*)$/);
  return m ? { path: m[2], dir: row!.classList.contains("dir") } : null;
}

/** On a highlighted session, that pane's text. Elsewhere, the app's. */
function fontKey(step: -1 | 0 | 1) {
  const id = fontTarget();
  if (id) return stepPaneFont(id, step);
  setFontSize(step ? S.fontSize + step : FONT_DEFAULT);
}
