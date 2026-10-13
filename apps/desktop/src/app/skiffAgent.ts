import { invoke } from "@tauri-apps/api/core";
import { showError } from "../ui/alerts";
import type { AgentInfo, SessionInfo } from "../platform/types";
import { newSession } from "./daemon";
import { setSkiffAgentName } from "./keys";
import { render } from "./renderRequest";
import { S, sessions } from "./state";
import { continueLine } from "../terminal/resume";
import { agentKind } from "../appearance/agentIcon";
import { byStart, samePath } from "../workspace/model";
import { keepRow } from "../workspace/selection";
import { revealSession } from "../workspace/view";

/** The name until Settings has one. skiffd's setup keeps the same default. */
export const DEFAULT_NAME = "Skipper";

/** The row key while no Skiff agent session runs. */
export const SKIFF_AGENT_KEY = "skiff-agent";

/** `[skiff_agent]` in projects.toml. */
export interface SkiffAgentSettings {
  name: string | null;
  agent: string | null;
  /** The row shows and the folder is set up. */
  enabled: boolean;
  /** `~/skiff-agent`, or the override in projects.toml. Null when the override is refused. */
  folder: string | null;
  /** Why the override is refused. */
  folder_error: string | null;
  continue_on_restore: boolean;
}

/** The folder while the Skiff agent is on and its folder is good. */
export function skiffAgentFolder(): string | null {
  return S.skiffAgent?.enabled ? S.skiffAgent.folder : null;
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

/** The session the row started. A shell or another agent opened in the folder does not take its place. */
let startedId: string | null = null;

/**
 * The Skiff agent's session: the one the row started while it lives. After a
 * restart that id is gone: then the oldest agent session in the folder.
 */
export function skiffAgentSession(): SessionInfo | null {
  const folder = skiffAgentFolder();
  if (!folder) return null;
  const started = startedId ? sessions.get(startedId) : undefined;
  if (started && started.state !== "done" && samePath(started.cwd, folder)) return started;
  return [...sessions.values()].filter((s) => s.state !== "done" && s.role === "agent" && samePath(s.cwd, folder)).sort(byStart)[0] ?? null;
}

/** True for the Skiff agent's session. It has no project, no splits and no rename: its name lives in Settings. */
export const isSkiffAgent = (id: string | null | undefined) => !!id && skiffAgentSession()?.id === id;

/** The row and the key: show the Skiff agent's pane, or start it in its folder. */
export function openSkiffAgent() {
  const folder = skiffAgentFolder();
  if (!folder) return;
  const s = skiffAgentSession();
  if (s) {
    revealSession(s.id);
    requestAnimationFrame(() => keepRow(s.id));
    return;
  }
  const a = skiffAgentChoice();
  if (!a) return showError("No agent is on. Turn one on in Settings › Agents.");
  // The folder is made ready first: files the user deleted come back.
  invoke<string>("ensure_skiff_agent")
    .then((dir) => newSession(dir, a.command, a.id, "agent", undefined, skiffAgentName()))
    .then((id) => {
      startedId = id;
      watch(id);
    })
    .catch(showError);
}

/** When the app last asked for a watcher, so a render does not ask each time. */
let watchAsked = { id: "", at: 0 };

/**
 * Starts `skiff watch` for the Skiff agent's session: on start, and again after
 * an app restart or a daemon restart. The backend starts none while one runs,
 * so this asks again now and then.
 */
function watch(id: string) {
  const now = Date.now();
  if (watchAsked.id === id && now - watchAsked.at < 30_000) return;
  watchAsked = { id, at: now };
  invoke<boolean>("start_skiff_watch", { session: id }).catch(console.error);
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
  watch(s.id);
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
