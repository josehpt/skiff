import { h } from "../ui/dom";
import type { AgentInfo } from "../platform/types";
import type { SkiffAgentSettings } from "../app/skiffAgent";
import { DEFAULT_NAME } from "../app/skiffAgent";
import { continueLine } from "../terminal/resume";

/** One edit, as `set_skiff_agent` takes it. */
export type SkiffAgentEdit =
  | { key: "name" | "agent" | "folder"; value: string | null }
  | { key: "continue_on_restore"; value: boolean };

/**
 * Settings › Skiff agent: its name, the agent it starts, its folder, and
 * whether a restored pane continues the last conversation.
 */
export function skiffAgentSection(set: SkiffAgentSettings, agents: AgentInfo[], save: (e: SkiffAgentEdit) => void): HTMLElement[] {
  // Enabled agents and the user's own. A custom agent is on when the user added it.
  const choices = agents.filter((a) => a.enabled || a.custom);
  const chosen = choices.find((a) => a.id === set.agent && a.installed) ?? agents.find((a) => a.enabled) ?? null;

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
  const note = h(
    "div",
    "set-note",
    "A running session keeps its agent. A new agent starts a new conversation: the old agent's history stays in the folder, but the new agent cannot read it. The ledger carries the open tasks across.",
  );

  const folder = textRow("Folder", set.folder ?? "", "none: no row in the side panel", (v) => save({ key: "folder", value: v || null }));

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
  const contNote = h(
    "div",
    "set-note",
    "Off: a restored pane is a shell, as every other pane is. On: Skiff types the line above into the restored Skiff agent pane, and only into that pane.",
  );
  return [name, agentRow, note, folder, cont, contNote];
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
