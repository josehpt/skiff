import { invoke } from "@tauri-apps/api/core";
import { promptAction } from "../ui/confirm";
import { showError, showNotice } from "../ui/alerts";
import type { AgentInfo, SessionInfo } from "../platform/types";
import { newSession } from "./daemon";
import { setSkiffAgentName } from "./keys";
import { render } from "./renderRequest";
import { S, sessions } from "./state";
import { continueLine } from "../terminal/resume";
import { agentKind } from "../appearance/agentIcon";
import { byStart, samePath } from "../workspace/model";
import { keepRow } from "../workspace/selection";
import { refocusTerminal, revealSession } from "../workspace/view";

/** The default folder. It does not follow the agent's name, so a rename keeps its history. */
const FOLDER = "~/skiff-agent";

/** The name until Settings has one. skiffd's setup keeps the same default. */
export const DEFAULT_NAME = "Skipper";

/** The row key while no Skiff agent session runs. */
export const SKIFF_AGENT_KEY = "skiff-agent";

/** `[skiff_agent]` in projects.toml. */
export interface SkiffAgentSettings {
  name: string | null;
  agent: string | null;
  folder: string | null;
  continue_on_restore: boolean;
}

/** Reads `[skiff_agent]` again. loadProjects calls it, so an edit from Settings or the CLI shows at once. */
export async function loadSkiffAgent() {
  const a = await invoke<SkiffAgentSettings>("get_skiff_agent").catch(() => null);
  S.skiffAgent = a;
  setSkiffAgentName(skiffAgentName());
}

export function skiffAgentName(): string {
  return S.skiffAgent?.name?.trim() || DEFAULT_NAME;
}

/**
 * The agent the row starts: the one from Settings while it is installed and
 * on (a custom agent is on), else the first enabled one. Settings shows the same.
 */
export function skiffAgentChoice(agents: AgentInfo[] = S.agents, id = S.skiffAgent?.agent): AgentInfo | null {
  return agents.find((a) => a.id === id && a.installed && (a.enabled || a.custom)) ?? agents.find((a) => a.enabled) ?? null;
}

/**
 * The Skiff agent's session: the oldest live one in its folder. An agent
 * session goes before a shell there, so a shell opened in the folder does
 * not take the row from the agent.
 */
export function skiffAgentSession(): SessionInfo | null {
  const folder = S.skiffAgent?.folder;
  if (!folder) return null;
  const here = [...sessions.values()].filter((s) => s.state !== "done" && samePath(s.cwd, folder)).sort(byStart);
  return here.find((s) => s.role === "agent") ?? here[0] ?? null;
}

/** The row and the key: show the Skiff agent's pane, or start it in its folder. */
export function openSkiffAgent() {
  const folder = S.skiffAgent?.folder;
  if (!folder) return;
  const s = skiffAgentSession();
  if (s) {
    revealSession(s.id);
    requestAnimationFrame(() => keepRow(s.id));
    return;
  }
  const a = skiffAgentChoice();
  if (!a) return showError("No agent is on. Turn one on in Settings › Agents.");
  newSession(folder, a.command, a.id, "agent", undefined, skiffAgentName()).catch(showError);
}

/** Restored panes Skiff already typed the continue line into. Once each. */
const continued = new Set<string>();

/**
 * Runs on each render. A restored Skiff agent pane continues the last
 * conversation in its folder when Settings says so. Only this pane: every
 * other pane restores as a shell and the user picks. A custom agent has no
 * continue line. A rename from Settings renames the running session too.
 */
export function syncSkiffAgent() {
  // Not read yet: the default name is not a rename.
  if (!S.skiffAgent) return;
  const s = skiffAgentSession();
  const name = skiffAgentName();
  // Only a change in Settings renames it. A rename of the pane itself stays.
  if (s && shownName !== null && shownName !== name && s.name !== name) rename(s.id, name);
  shownName = name;
  if (!s) return;
  if (!S.skiffAgent.continue_on_restore || !s.resume || continued.has(s.id) || agentKind(s) !== "shell") return;
  const a = skiffAgentChoice();
  const line = a && continueLine(a.id, a.command);
  if (!line) return;
  continued.add(s.id);
  invoke("pty_write", { session: s.id, data: line + "\r" }).catch(showError);
}

/** The name the last render saw, so a change from Settings shows. */
let shownName: string | null = null;

function rename(id: string, name: string) {
  invoke<SessionInfo>("rename_session", { session: id, name })
    .then((info) => {
      const s = sessions.get(id);
      if (s) s.name = info.name;
      render();
    })
    .catch(console.error);
}

/**
 * Writes the Skiff agent template into an empty folder the user names. The
 * folder becomes the Skiff agent's, so its row shows above the projects.
 */
export function setUpSkiffAgent() {
  let dir: string | null = null;
  promptAction({
    title: "Set up Skiff agent",
    body: "Skiff writes the agent's instructions, its task ledger and a results folder into an empty folder. It does not overwrite files. The folder becomes the Skiff agent's folder in Settings.",
    placeholder: "a folder",
    value: S.skiffAgent?.folder ? "" : FOLDER,
    action: "Set up",
    submit: async (path) => {
      dir = await invoke<string>("setup_skiff_agent", { path });
    },
    onClose: () => {
      refocusTerminal();
      if (dir) showNotice(`Skiff wrote the agent's files to ${dir}. Select ${skiffAgentName()} at the top of the side panel to start it.`);
    },
  });
}
