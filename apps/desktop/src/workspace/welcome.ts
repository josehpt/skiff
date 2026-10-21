import { launchIcon } from "../appearance/agentIcon";

import { branchName } from "./model";

import { newSession } from "../app/daemon";

import { button, h } from "../ui/dom";
import { branchIcon } from "../ui/icons";
import { host } from "../terminal/terminalHost";

import { showError } from "../ui/alerts";
import { newWorktree } from "./menus";

import { addProject } from "../app/panels";
import { openClosedProject } from "./projectClose";

import { S } from "../app/state";
import { hostName, isRemote } from "../app/hosts";
import { accent, currentProject, launchChoices, shownIds } from "../app/stateQueries";

let welcomeSig = "";
let welcomeBox: HTMLElement | null = null;

/** First run: add a project. Just added, or nothing focused: start an agent. */
export function renderWelcome() {
  const p = currentProject();
  const w = p?.worktrees.find((x) => x.is_main) ?? p?.worktrees[0];
  // Same content: keep the element, and the focus and hover on its buttons.
  const sig = JSON.stringify([S.projects.length, S.closedProjects.map((x) => x.name), !!S.projectsError, p?.name, p?.color, w?.path, S.justAdded === p?.name, !!S.focused, shownIds().length, launchChoices().map((a) => a?.id)]);
  if (sig === welcomeSig && (!welcomeBox || welcomeBox.isConnected)) return;
  welcomeSig = sig;
  host.querySelector(".empty")?.remove();
  welcomeBox?.remove();
  welcomeBox = null;
  const box = h("div", "welcome");
  if (isRemote()) {
    // A remote Host: its sessions, and work starts from its own CLI (M2).
    if (S.focused || shownIds().length) return;
    box.append(
      h("div", "welcome-title", `Sessions on ${hostName()}`),
      h("div", "welcome-text", `Pick a session in the list. To start work, run skiff on ${hostName()}, for example: skiff new --agent claude.`),
    );
  } else if (S.projects.length === 0 && S.closedProjects.length && !S.projectsError) {
    // Every project is closed: open one again, or add another.
    const actions = h("div", "welcome-actions");
    for (const c of S.closedProjects) {
      const b = button("", c.name, () => void openClosedProject(c));
      b.style.color = accent(c);
      actions.appendChild(b);
    }
    const add = button("", "add project…", () => void addProject.open());
    actions.appendChild(add);
    box.append(
      h("div", "welcome-title", "No open project"),
      h("div", "welcome-text", "Open a closed project again, with its settings, or add another."),
      actions,
    );
  } else if (S.projects.length === 0 && !S.projectsError) {
    const add = button("welcome-primary", "Add project", () => void addProject.open());
    const hint = h("div", "welcome-hint", "or edit ");
    hint.appendChild(h("span", "mono", "~/.config/skiff/projects.toml"));
    box.append(
      h("div", "welcome-title", "Add your first project"),
      h("div", "welcome-text", "Pick a folder in a git repository. Skiff lists its worktrees and runs agents in them."),
      add,
      hint,
    );
  } else if (p && w && !shownIds().length && (S.justAdded === p.name || !S.focused)) {
    const title = h("div", "welcome-title");
    const name = h("span", "", p.name);
    name.style.color = accent(p);
    title.append(name, S.justAdded === p.name ? " is ready" : "");
    const text = h("div", "welcome-text", "Start an agent in ");
    text.append(h("span", "mono", branchName(w)), ", or make a worktree for a task first.");
    const actions = h("div", "welcome-actions");
    for (const a of launchChoices()) {
      const b = button("", "", () => {
        S.justAdded = null;
        newSession(w.path, a?.command ?? null, a?.id ?? "", a ? "agent" : "shell").catch(showError);
      });
      b.append(launchIcon(a), a ? a.id : "shell");
      actions.appendChild(b);
    }
    const wt = button("", "", () => newWorktree(p));
    wt.append(branchIcon(), "worktree…");
    actions.appendChild(wt);
    box.append(title, text, actions);
  } else if (!S.focused && !shownIds().length) {
    box.append(h("div", "welcome-text", "No session. Press + on a worktree to start one."));
  } else {
    return;
  }
  host.appendChild(box);
  welcomeBox = box;
}
