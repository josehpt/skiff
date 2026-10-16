import { h } from "../ui/dom";
import type { AgentInfo } from "../platform/types";
import type { SkiffAgentSettings, TaskType } from "../app/skiffAgent";
import { DEFAULT_NAME, skiffAgentChoice } from "../app/skiffAgent";
import { continueLine } from "../terminal/resume";
import { rowTable, type RowTableState } from "./rowTable";
import { BAD_TASK_TYPE_NAME, modelError, modelFlag, taskCommand, TASK_TYPE_NAME } from "./taskTypes";
import { countsLine } from "./skiffFolderDialog";
import { reveal } from "../platform/fileActions";

/** A task type row as `set_skiff_agent` takes it. Null model: the agent's default. */
export interface TaskEdit {
  type: string;
  agent: string;
  model: string | null;
}

/** One edit, as `set_skiff_agent` takes it. */
export type SkiffAgentEdit =
  | { key: "name" | "agent"; value: string | null }
  | { key: "enabled" | "continue_on_restore"; value: boolean }
  | { key: "add_task" | "task"; value: TaskEdit }
  | { key: "rename_task"; value: { from: string; to: string } }
  | { key: "remove_task"; value: string };

/**
 * Settings › Skiff agent: on or off, its folder, its name, the agent it
 * starts, and whether a restored pane continues the last conversation.
 * `changeFolder` opens the Change… dialog: a change copies, checks and
 * switches, so the ledger and results go with the folder.
 */
export function skiffAgentSection(
  set: SkiffAgentSettings,
  agents: AgentInfo[],
  save: (e: SkiffAgentEdit) => void,
  changeFolder: () => void,
): HTMLElement[] {
  // Enabled agents and the user's own. A custom agent is on when the user added it.
  const choices = agents.filter((a) => a.enabled || a.custom);
  const chosen = skiffAgentChoice(agents, set.agent);

  const name = textRow("Name", set.name ?? "", DEFAULT_NAME, (v) => save({ key: "name", value: v || null }));

  const agentRow = h("div", "set-row set-open set-agent on");
  const pick = h("select", "c-cmd");
  pick.setAttribute("aria-label", "Skiff agent's agent");
  if (!choices.length) pick.append(h("option", "", "No agent is on"));
  for (const a of choices) {
    const o = h("option", "", a.custom ? `${a.id} (custom)` : a.id);
    o.value = a.id;
    pick.append(o);
  }
  if (chosen) pick.value = chosen.id;
  pick.disabled = !choices.length;
  pick.addEventListener("change", () => save({ key: "agent", value: pick.value }));
  agentRow.append(h("span", "c-name", "Agent"), pick, h("span", "c-mode", "At next start"));
  // On sets up the folder and shows the row. A refused override stays off: the Folder row says why.
  const onRow = switchRow("On", set.enabled && !!set.folder, !!set.folder, (on) => save({ key: "enabled", value: on }));
  onRow.insertBefore(h("span", "c-cmd", set.folder ? "" : "Change the folder to turn it on."), onRow.lastChild);

  // A custom agent has no continue line: the switch shows off, with the reason.
  const line = chosen ? continueLine(chosen.id, chosen.command) : null;
  const on = !!line && set.continue_on_restore;
  const cont = h("div", "set-row set-open set-agent" + (on ? " on" : "") + (line ? "" : " missing"));
  const sw = h("button", "c-mode set-switch");
  sw.type = "button";
  sw.disabled = !line;
  sw.setAttribute("role", "switch");
  sw.setAttribute("aria-checked", String(on));
  sw.setAttribute("aria-label", `Continue the last conversation on restore ${on ? "on" : "off"}`);
  sw.appendChild(h("span", "switch"));
  sw.addEventListener("click", () => save({ key: "continue_on_restore", value: !on }));
  const what = line
    ? h("span", "c-cmd mono", line)
    : h("span", "c-cmd", chosen ? `${chosen.id} has no continue command. The pane restores as a shell.` : "No agent is on.");
  cont.append(h("span", "c-name", "Continue on restore"), what, sw);
  return [onRow, ...folderRows(set, changeFolder), name, agentRow, cont];
}

/** The Folder row and its status line: the path to copy, Show in file manager and Change…. */
function folderRows(set: SkiffAgentSettings, change: () => void): HTMLElement[] {
  const row = h("div", "set-row set-open set-agent set-folder on");
  const path = h("span", "c-cmd mono set-path", set.configured);
  path.title = set.configured;
  const acts = h("span", "c-mode set-folder-acts");
  if (set.folder_status === "ready") {
    const show = h("button", "set-rm", "Show in file manager");
    show.type = "button";
    show.addEventListener("click", () => reveal(set.configured));
    acts.append(show);
  }
  const edit = h("button", "set-rm", "Change…");
  edit.type = "button";
  edit.addEventListener("click", change);
  acts.append(edit);
  row.append(h("span", "c-name", "Folder"), path, acts);

  const words: Record<SkiffAgentSettings["folder_status"], string> = {
    ready: `Ready · ${set.folder_counts ? countsLine(set.folder_counts) : ""}`,
    new: "New: Skiff makes it at On.",
    other: "Not usable: it holds other files. Choose another folder.",
    refused: set.folder_error ?? "Refused.",
  };
  const meta = h("div", `set-folder-meta ${set.folder_status}`);
  meta.append(h("span", "sq"), h("span", "", words[set.folder_status]));
  return [row, meta];
}

/**
 * Settings › Skiff agent › Task types: the agent and model Skipper starts
 * for each kind of task. `save` answers with the error to show, or null.
 */
export function taskTypesSection(
  set: SkiffAgentSettings,
  agents: AgentInfo[],
  state: RowTableState,
  save: (e: SkiffAgentEdit) => Promise<string | null>,
  redraw: () => void,
): HTMLElement[] {
  // As the Agent row above: enabled agents and the user's own.
  const choices = agents.filter((a) => a.enabled || a.custom);
  const byId = (id: string) => agents.find((a) => a.id === id);
  const label = (a: AgentInfo) => (a.custom ? `${a.id} (custom)` : a.id);
  const head = h("div", "set-head");
  const title = h("div", "set-h", "Task types");
  title.id = "set-h-tasks";
  head.append(
    title,
    h("div", "set-sub", "Skipper starts a task of this type with this agent and model. Empty model: the agent's default. A model named in the task wins."),
  );
  const row = (t: TaskType, change: Partial<TaskEdit>): TaskEdit => ({ type: t.type, agent: t.agent, model: t.model, ...change });
  const table = rowTable<TaskType>(
    {
      grid: "g-tasks",
      noun: "task type",
      article: "A task type",
      dupHint: "Change it in its row.",
      labelledBy: title.id,
      rows: set.tasks ?? [],
      id: (t) => t.type,
      cols: [
        {
          key: "type",
          head: "TYPE",
          name: "type",
          kind: "key",
          required: true,
          get: (t) => t.type,
          check: (v) => (TASK_TYPE_NAME.test(v) ? null : BAD_TASK_TYPE_NAME),
        },
        {
          key: "agent",
          head: "AGENT",
          name: "agent",
          kind: "select",
          empty: "No agent is on",
          options: (t) => {
            const list: [string, string][] = choices.map((a) => [a.id, label(a)]);
            // A row keeps an agent that is off or gone, so the select shows the truth.
            if (t && !choices.some((a) => a.id === t.agent)) list.unshift([t.agent, `${t.agent} (${byId(t.agent) ? "off" : "gone"})`]);
            return list;
          },
          initial: () => skiffAgentChoice(agents, set.agent)?.id ?? choices[0]?.id ?? "",
          get: (t) => t.agent,
        },
        {
          key: "model",
          head: "MODEL",
          name: "model",
          kind: "text",
          mono: true,
          placeholder: () => "agent's default",
          get: (t) => t.model ?? "",
          // A custom agent has its model in its command. A gone agent has none.
          note: (t, d) => {
            const a = byId(t ? t.agent : (d.agent ?? ""));
            return modelFlag(a) ? null : a ? "in the command" : "";
          },
          check: modelError,
        },
        {
          key: "command",
          head: "COMMAND",
          name: "command",
          kind: "preview",
          get: (t) => t.command ?? t.error ?? "",
          bad: (t) => !t.command,
        },
      ],
      preview: (d) => taskCommand(byId(d.agent), d.model),
      blocked: () => (choices.length ? null : "Turn on an agent in Settings › Agents."),
      add: (d) => save({ key: "add_task", value: { type: d.type, agent: d.agent, model: d.model || null } }),
      edit: (t, key, v) => {
        if (key === "type") return save({ key: "rename_task", value: { from: t.type, to: v } });
        // A new agent with no model flag drops the model: skiff-core saves none.
        return save({ key: "task", value: row(t, key === "agent" ? { agent: v } : { model: v || null }) });
      },
      remove: (t) => save({ key: "remove_task", value: t.type }),
    },
    state,
    redraw,
  );
  return [head, table];
}

/** A label, a cell for the caller, and a switch. */
function switchRow(label: string, on: boolean, enabled: boolean, set: (on: boolean) => void): HTMLElement {
  const row = h("div", "set-row set-open set-agent" + (on ? " on" : "") + (enabled ? "" : " missing"));
  const sw = h("button", "c-mode set-switch");
  sw.type = "button";
  sw.disabled = !enabled;
  sw.setAttribute("role", "switch");
  sw.setAttribute("aria-checked", String(on));
  sw.setAttribute("aria-label", `${label} ${on ? "on" : "off"}`);
  sw.appendChild(h("span", "switch"));
  sw.addEventListener("click", () => set(!on));
  row.append(h("span", "c-name", label), sw);
  return row;
}

/** A label and a text field that saves on Enter or blur. Esc puts the old value back. */
function textRow(label: string, value: string, placeholder: string, save: (v: string) => void): HTMLElement {
  const row = h("div", "set-row set-open set-agent on");
  const input = h("input", "c-cmd");
  input.value = value;
  input.placeholder = placeholder;
  input.spellcheck = false;
  input.setAttribute("aria-label", `Skiff agent ${label.toLowerCase()}`);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      input.blur();
    } else if (e.key === "Escape") {
      input.value = value;
      input.blur();
    }
  });
  input.addEventListener("blur", () => {
    if (input.value.trim() !== value.trim()) save(input.value.trim());
  });
  row.append(h("span", "c-name", label), input, h("span", "c-mode", ""));
  return row;
}
