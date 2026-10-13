import { DEFAULT_THEME } from "./colors";
import { escapeButton } from "../ui/dialogParts";
import { h } from "../ui/dom";
import { rune } from "../ui/runes";
import { invoke } from "@tauri-apps/api/core";
import type { AgentInfo, TerminalTheme } from "../platform/types";
import { agentNameList, setAgentNameList } from "./agentNames";
import { nextCard, themeGrid, type ThemeFilters } from "./themeCards";
import { type Action, actionFor } from "../app/keys";
import type { SkiffAgentSettings } from "../app/skiffAgent";
import { type SkiffAgentEdit, skiffAgentSection } from "./skiffAgentSettings";

/**
 * Settings over the sidebar and terminal: agents, appearance, and Open with.
 */
export interface AppearanceHooks {
  themes: () => TerminalTheme[];
  /** A theme id, or null for the default Foot theme. */
  current: () => string | null;
  load: () => Promise<void>;
  set: (id: string | null) => void;
}

export function createSettings(onAgents: (a: AgentInfo[]) => void, onClose: () => void, look: AppearanceHooks) {
  const root = h("div");
  root.id = "settings";
  root.hidden = true;
  root.setAttribute("role", "dialog");
  root.setAttribute("aria-label", "Settings");
  document.getElementById("body")!.appendChild(root);

  let agents: AgentInfo[] = [];
  const TABS = ["agents", "skiff-agent", "appearance", "open"] as const;
  let tab: (typeof TABS)[number] = "agents";
  const themeFilters: ThemeFilters = { query: "", family: null };
  type OpenKey = "diff" | "text" | "markdown" | "html";
  type OpenSettings = Record<OpenKey, string> & { default_diff: string; peek: string[] };
  let openSet: OpenSettings | null = null;
  type MenuLayout = { available: boolean; chosen: string | null; auto: string; current: string };
  let menuSet: MenuLayout | null = null;
  let agentSet: SkiffAgentSettings | null = null;
  let error: string | null = null;
  let saving = false;

  async function toggle(id: string) {
    if (saving) return;
    const enabled = agents.filter((a) => (a.id === id ? !a.enabled : a.enabled)).map((a) => a.id);
    saving = true;
    error = null;
    // Show the change at once; the daemon's answer replaces it.
    agents = agents.map((a) => (a.id === id ? { ...a, enabled: !a.enabled } : a));
    render();
    try {
      agents = await invoke<AgentInfo[]>("set_agents", { enabled });
      onAgents(agents);
    } catch (e) {
      error = String(e);
      agents = await invoke<AgentInfo[]>("list_agents").catch(() => agents);
    } finally {
      saving = false;
      render();
    }
  }

  /** Sends an agent edit to the daemon and shows the list it answers with. */
  async function editAgents(cmd: string, args: Record<string, string> = {}) {
    error = null;
    try {
      agents = await invoke<AgentInfo[]>(cmd, args);
      onAgents(agents);
    } catch (e) {
      error = String(e);
    }
    render();
  }

  const setCommand = (agent: string, command: string) => editAgents("set_agent_command", { agent, command });

  /** Removes the agent. The keys go to the row that takes its place. */
  async function removeAgent(agent: string) {
    const at = agents.findIndex((a) => a.id === agent);
    await editAgents("remove_agent", { agent });
    const rows = root.querySelectorAll<HTMLElement>(".set-agent:not(.set-add) .set-rm");
    (rows[Math.min(at, rows.length - 1)] ?? root.querySelector<HTMLElement>(".set-add input"))?.focus();
  }

  function render() {
    // A rebuild keeps the list where it was scrolled.
    const scroll = root.querySelector(".set-main")?.scrollTop ?? 0;
    // So does the focused field: a save on blur finishes after the click that moved focus to the next field.
    const a = document.activeElement;
    const field = a instanceof HTMLInputElement && root.contains(a) ? { label: a.getAttribute("aria-label"), at: a.selectionStart, to: a.selectionEnd } : null;
    draw();
    const main = root.querySelector(".set-main");
    if (main) main.scrollTop = scroll;
    if (field?.label) {
      const next = [...root.querySelectorAll<HTMLInputElement>("input")].find((i) => i.getAttribute("aria-label") === field.label);
      next?.focus();
      if (next && field.at !== null && field.to !== null) next.setSelectionRange(field.at, field.to);
    }
  }

  function draw() {
    const nav = h("div", "set-nav");
    const head = h("div", "set-nav-head");
    head.append(h("div", "set-title", "Settings"), h("div", "set-path mono", "~/.config/skiff/projects.toml"));
    const names = { agents: "Agents", "skiff-agent": "Skiff agent", appearance: "Appearance", open: "Open with" };
    const tabs = TABS.map((t) => {
      const b = h("button", "set-tab" + (tab === t ? " active" : ""), names[t]);
      b.type = "button";
      b.addEventListener("click", () => {
        tab = t;
        draw();
        if (tab === "appearance") root.querySelector<HTMLInputElement>(".tc-search")?.focus();
      });
      return b;
    });
    nav.append(head, ...tabs);
    if (tab === "appearance") {
      root.replaceChildren(nav, appearance(), closeButton());
      return;
    }
    if (tab === "open") {
      root.replaceChildren(nav, openWith(), closeButton());
      return;
    }
    if (tab === "skiff-agent") {
      root.replaceChildren(nav, skiffAgent(), closeButton());
      return;
    }

    const main = h("div", "set-main");
    const intro = h("div", "set-head");
    intro.append(h("div", "set-h", "Agents"));
    // Only a removed default can come back.
    if (DEFAULT_AGENTS.some((d) => !agents.some((a) => a.id === d))) {
      intro.classList.add("set-head-row");
      const back = h("button", "set-rm", "Restore defaults");
      back.type = "button";
      back.addEventListener("click", () => void editAgents("restore_agents"));
      intro.appendChild(back);
    }
    const cols = h("div", "set-row set-cols");
    cols.append(h("span", "c-on", ""), h("span", "c-name", "AGENT"), h("span", "c-cmd", "COMMAND"), h("span", "c-status", "STATUS"));
    main.append(intro, cols);
    for (const a of agents) {
      const row = h("div", "set-row set-agent" + (a.enabled ? " on" : "") + (a.installed ? "" : " missing"));
      const sw = h("button", "c-on");
      sw.type = "button";
      sw.disabled = !a.installed;
      sw.setAttribute("role", "switch");
      sw.setAttribute("aria-checked", String(a.enabled));
      sw.setAttribute("aria-label", `${a.id} ${a.enabled ? "on" : "off"}`);
      sw.title = a.installed ? "" : `${a.command.split(/\s+/)[0]} is not on PATH`;
      sw.appendChild(h("span", "switch"));
      sw.addEventListener("click", () => void toggle(a.id));

      const cmd = h("input", "c-cmd mono");
      // A known agent shows its default as the placeholder. A custom one has no default.
      const cur = a.custom || a.command !== a.default_command ? a.command : "";
      cmd.value = cur;
      cmd.placeholder = a.default_command;
      cmd.spellcheck = false;
      cmd.setAttribute("aria-label", `${a.id} command`);
      const save = () => {
        const v = cmd.value.trim();
        // Remove takes a custom agent away. An empty field does not.
        if (a.custom && !v) cmd.value = cur;
        else if (v !== cur) void setCommand(a.id, v);
      };
      cmd.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          cmd.blur();
        } else if (e.key === "Escape") {
          cmd.value = cur;
          cmd.blur();
        }
      });
      cmd.addEventListener("blur", save);

      const name = h("input", "c-name");
      name.value = a.id;
      name.spellcheck = false;
      name.setAttribute("aria-label", `${a.id} name`);
      name.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          name.blur();
        } else if (e.key === "Escape") {
          name.value = a.id;
          name.blur();
        }
      });
      name.addEventListener("blur", () => {
        const v = name.value.trim();
        if (!v) name.value = a.id;
        else if (v !== a.id) void editAgents("rename_agent", { agent: a.id, name: v });
      });

      const status = h("span", "c-status");
      status.append(h("span", "sq"), a.installed ? "installed" : "not on PATH");
      const rm = h("button", "set-rm set-ico");
      rm.type = "button";
      rm.title = "Remove";
      rm.setAttribute("aria-label", `Remove ${a.id}`);
      rm.appendChild(rune("indicators-minus"));
      rm.addEventListener("click", () => void removeAgent(a.id));
      status.appendChild(rm);
      row.append(sw, name, cmd, status);
      main.appendChild(row);
    }
    main.appendChild(addRow());
    if (error) {
      const err = h("div", "set-error", error);
      err.setAttribute("role", "alert");
      main.appendChild(err);
    }
    main.appendChild(callSigns());
    root.replaceChildren(nav, main, closeButton());
  }

  /** The agents Skiff ships with. Restore defaults brings back any the user removed. */
  const DEFAULT_AGENTS = ["claude", "codex", "gemini", "grok", "opencode"];

  /** A blank row that adds a custom agent: a name and the command line it runs. */
  function addRow(): HTMLElement {
    const row = h("div", "set-row set-agent set-add");
    const name = h("input", "c-name");
    name.placeholder = "Add agent";
    name.spellcheck = false;
    name.setAttribute("aria-label", "New agent name");
    const cmd = h("input", "c-cmd mono");
    cmd.placeholder = "command, for example aider --model sonnet";
    cmd.spellcheck = false;
    cmd.setAttribute("aria-label", "New agent command");
    const add = h("button", "set-rm set-ico");
    add.type = "button";
    add.title = "Add";
    add.setAttribute("aria-label", "Add agent");
    add.appendChild(rune("indicators-plus"));
    const submit = () => {
      const id = name.value.trim();
      const command = cmd.value.trim();
      if (!id || !command) return (id ? cmd : name).focus();
      if (agents.some((a) => a.id === id)) {
        error = `An agent named ${id} exists. Change its command in its row.`;
        return render();
      }
      void setCommand(id, command).then(() => {
        // A skiffd from before custom agents saves the line but does not list it.
        if (error || agents.some((a) => a.id === id)) return;
        error = `Saved ${id} to projects.toml. Restart skiffd to show it.`;
        render();
      });
    };
    for (const f of [name, cmd])
      f.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          submit();
        } else if (e.key === "Escape" && f.value) {
          e.stopPropagation();
          name.value = cmd.value = "";
        }
      });
    add.addEventListener("click", submit);
    const end = h("span", "c-status");
    end.appendChild(add);
    row.append(h("span", "c-on"), name, cmd, end);
    return row;
  }

  /** Esc, in the top right corner as in the theme picker. A click closes. */
  function closeButton(): HTMLButtonElement {
    return escapeButton("set-x", "Close settings", close);
  }

  /** The words that make up the names of new agent sessions. */
  function callSigns(): HTMLElement {
    const box = h("div", "set-names-box");
    const head = h("div", "set-head");
    head.append(h("div", "set-h", "Agent names"));
    box.appendChild(head);
    const cols = h("div", "set-names-cols");
    for (const [list, label] of [["ranks", "TITLES"], ["nouns", "NAMES"]] as const) {
      const col = h("div", "set-names-col");
      const area = h("textarea", "mono");
      area.spellcheck = false;
      area.value = agentNameList(list).join("\n");
      area.setAttribute("aria-label", `Agent ${label.toLowerCase()}`);
      area.addEventListener("blur", () => {
        setAgentNameList(list, area.value.split("\n"));
        area.value = agentNameList(list).join("\n");
      });
      col.append(h("div", "set-names-label", label), area);
      cols.appendChild(col);
    }
    box.appendChild(cols);
    return box;
  }

  /** App colors made from a terminal theme. A click applies it. */
  function appearance(): HTMLElement {
    const main = h("div", "set-main");
    const intro = h("div", "set-head");
    intro.append(
      h("div", "set-h", "Appearance"),
      h("div", "set-sub", "App theme. Colors the app and every project and session without a theme of its own."),
    );
    const cur = look.current()?.replace(/^foot:/, "builtin:") ?? null;
    const cards = h("div", "set-cards");
    cards.appendChild(
      themeGrid(look.themes(), {
        active: look.themes().some((t) => t.id === cur) ? cur : DEFAULT_THEME,
        filters: themeFilters,
        reload: async () => { await look.load(); return look.themes(); },
        pick: (id) => {
          look.set(id === DEFAULT_THEME ? null : id);
        },
      }),
    );
    main.append(intro);
    if (menuSet?.available) main.appendChild(menuLayout(menuSet));
    main.appendChild(cards);
    return main;
  }

  /** Linux: the GNOME header bar or the menu bar. GTK sets it when the window opens. */
  function menuLayout(m: MenuLayout): HTMLElement {
    const row = h("div", "set-row set-opacity set-menu");
    const pick = h("div", "c-cmd set-seg");
    pick.setAttribute("role", "radiogroup");
    pick.setAttribute("aria-label", "Menu layout");
    const auto = m.auto === "header-bar" ? "Auto (header bar)" : "Auto (menu bar)";
    const choices: [string | null, string][] = [[null, auto], ["header-bar", "Header bar"], ["menu-bar", "Menu bar"]];
    for (const [value, label] of choices) {
      const b = h("button", "set-seg-item" + (m.chosen === value ? " on" : ""), label);
      b.type = "button";
      b.setAttribute("role", "radio");
      b.setAttribute("aria-checked", String(m.chosen === value));
      b.addEventListener("click", () => void setMenuLayout(value));
      pick.appendChild(b);
    }
    const pending = (m.chosen ?? m.auto) !== m.current;
    row.append(h("span", "c-name", "Menu"), pick, h("span", "c-status", pending ? "On restart" : ""));
    return row;
  }

  async function setMenuLayout(layout: string | null) {
    error = null;
    try {
      menuSet = await invoke<MenuLayout>("set_menu_layout", { layout });
    } catch (e) {
      error = String(e);
    }
    render();
  }

  async function setOpen(key: OpenKey, command: string) {
    error = null;
    try {
      openSet = await invoke<OpenSettings>("set_open", { key, command });
    } catch (e) {
      error = String(e);
    }
    render();
  }

  async function setOpenPeek(key: OpenKey, peek: boolean) {
    error = null;
    try {
      openSet = await invoke<OpenSettings>("set_open_peek", { key, peek });
    } catch (e) {
      error = String(e);
    }
    render();
  }

  const OPEN_ROWS: { key: OpenKey; label: string }[] = [
    { key: "diff", label: "Diffs" },
    { key: "text", label: "Text files" },
    { key: "markdown", label: "Markdown" },
    { key: "html", label: "HTML" },
  ];

  async function setSkiffAgent(edit: SkiffAgentEdit) {
    error = null;
    try {
      agentSet = await invoke<SkiffAgentSettings>("set_skiff_agent", { edit });
    } catch (e) {
      error = String(e);
    }
    render();
  }

  /** The one agent session that coordinates the others. */
  function skiffAgent(): HTMLElement {
    const main = h("div", "set-main");
    const intro = h("div", "set-head");
    intro.append(
      h("div", "set-h", "Skiff agent"),
      h("div", "set-sub", "One agent for all projects."),
    );
    main.append(intro);
    if (agentSet) main.append(...skiffAgentSection(agentSet, agents, (e) => void setSkiffAgent(e)));
    if (error) {
      const err = h("div", "set-error", error);
      err.setAttribute("role", "alert");
      main.appendChild(err);
    }
    return main;
  }

  /** Which apps Skiff hands things to. */
  function openWith(): HTMLElement {
    const main = h("div", "set-main");
    const intro = h("div", "set-head");
    intro.append(
      h("div", "set-h", "Open with"),
      h("div", "set-sub", "Peek opens the command when you select a file. External app shows an Open button. Double-click or Enter opens the file. A file with no command uses the default app."),
    );
    const cols = h("div", "set-row set-open set-cols");
    cols.append(h("span", "c-name", "SHOWS"), h("span", "c-cmd", "COMMAND"), h("span", "c-mode", "OPENS IN"));
    main.append(intro, cols);
    const o = openSet;
    if (o) for (const { key, label } of OPEN_ROWS) {
      const row = h("div", "set-row set-open set-agent on");
      const cmd = h("input", "c-cmd mono");
      cmd.value = o[key];
      cmd.placeholder = key === "diff" ? o.default_diff : "default app";
      cmd.spellcheck = false;
      cmd.setAttribute("aria-label", `${label} command`);
      cmd.addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          cmd.blur();
        } else if (e.key === "Escape") {
          cmd.value = o[key];
          cmd.blur();
        }
      });
      cmd.addEventListener("blur", () => {
        if (cmd.value.trim() !== o[key].trim()) void setOpen(key, cmd.value);
      });
      row.append(h("span", "c-name", label), cmd);
      if (key === "diff") row.append(h("span", "c-mode", "Peek"));
      else {
        const mode = h("select", "c-mode");
        mode.setAttribute("aria-label", `${label} open mode`);
        for (const [value, label] of [["app", "External app"], ["peek", "Peek"]]) {
          const option = h("option", "", label);
          option.value = value;
          mode.append(option);
        }
        mode.value = o.peek.includes(key) ? "peek" : "app";
        mode.addEventListener("change", () => void setOpenPeek(key, mode.value === "peek"));
        row.append(mode);
      }
      main.appendChild(row);
    }
    if (error) {
      const err = h("div", "set-error", error);
      err.setAttribute("role", "alert");
      main.appendChild(err);
    }
    return main;
  }

  function close() {
    // Leaving a name list saves it.
    if (document.activeElement instanceof HTMLTextAreaElement) document.activeElement.blur();
    root.hidden = true;
    onClose();
  }

  /** Ctrl+Shift+Up/Down: the previous or next section, as on the rail. The keys go to its tab. */
  function stepTab(dir: 1 | -1) {
    // Leaving a field saves it, as a click on a tab does.
    (document.activeElement as HTMLElement | null)?.blur();
    tab = TABS[(TABS.indexOf(tab) + dir + TABS.length) % TABS.length];
    draw();
    document.body.classList.add("kbd");
    root.querySelector<HTMLButtonElement>(".set-tab.active")?.focus();
  }

  /** Ctrl+Shift+Right: into the section, at its first control. Ctrl+Shift+Left: back to its tab. */
  function enterSection(into: boolean) {
    document.body.classList.add("kbd");
    if (!into) return root.querySelector<HTMLButtonElement>(".set-tab.active")?.focus();
    if (tab === "appearance") return root.querySelector<HTMLInputElement>(".tc-search")?.focus();
    root.querySelector<HTMLElement>(".set-main :is(button, input, select, textarea, [tabindex]):not(:disabled)")?.focus();
  }

  /**
   * Plain arrows. On a tab, Up/Down change the section and Right goes into it.
   * In a section, Up/Down go to the nearest control above or below, and Left/Right
   * along the row. Text keeps its own keys: Left/Right move the caret, and a name
   * list lets Up/Down out only from its first or last line.
   */
  root.addEventListener("keydown", (e) => {
    if (e.ctrlKey || e.altKey || e.metaKey || e.shiftKey || !e.key.startsWith("Arrow")) return;
    const el = document.activeElement as HTMLElement | null;
    if (!el || !root.contains(el)) return;
    const up = e.key === "ArrowUp";
    const vertical = up || e.key === "ArrowDown";
    if (el.classList.contains("set-tab")) {
      if (vertical) stepTab(up ? -1 : 1);
      else if (e.key === "ArrowRight") enterSection(true);
      else return;
      return e.preventDefault();
    }
    if (!el.closest(".set-main")) return;
    if (el instanceof HTMLSelectElement) return;
    const text = el instanceof HTMLInputElement && el.type !== "range";
    // A text field moves its caret, and lets Left or Right out only at its start or end.
    if (!vertical && (el instanceof HTMLTextAreaElement || (el instanceof HTMLInputElement && !text))) return;
    if (!vertical && text) {
      const edge = e.key === "ArrowLeft" ? 0 : el.value.length;
      if (el.selectionStart !== edge || el.selectionEnd !== edge) return;
    }
    if (el instanceof HTMLTextAreaElement) {
      const v = el.value;
      if (up ? v.lastIndexOf("\n", el.selectionStart - 1) >= 0 : v.indexOf("\n", el.selectionEnd) >= 0) return;
    }
    const stops = [...root.querySelectorAll<HTMLElement>(".set-main :is(button, input, select, textarea):not(:disabled)")];
    const at = stops.indexOf(el);
    if (at < 0) return;
    e.preventDefault();
    let to = nextCard(stops, at, up ? "up" : e.key === "ArrowDown" ? "down" : e.key === "ArrowLeft" ? "left" : "right");
    // Left and Right keep to the row.
    if (!vertical && Math.abs(stops[to].getBoundingClientRect().top - el.getBoundingClientRect().top) > 1) to = at;
    // Left from the start of a row goes back to the tab.
    if (to === at && e.key === "ArrowLeft") return enterSection(false);
    const next = stops[to];
    next.focus();
    // Into a text field from the right, the caret starts at its end.
    if (next instanceof HTMLInputElement && next.type !== "range" && e.key === "ArrowLeft") next.setSelectionRange(next.value.length, next.value.length);
  });

  /** The list keys, as Settings reads them. True when Settings used the key. */
  function runKey(a: Action | null): boolean {
    if (a === "session-next" || a === "session-prev") stepTab(a === "session-next" ? 1 : -1);
    else if (a === "list-project" || a === "list-back") enterSection(a === "list-back");
    else return false;
    return true;
  }

  // On the document, so Escape works wherever focus is. Menus and dialogs
  // over settings stop the key first.
  document.addEventListener("keydown", (e) => {
    if (root.hidden || e.defaultPrevented || document.querySelector("#ctx-menu:not([hidden]), #launch-menu:not([hidden]), .confirm-overlay")) return;
    if (runKey(actionFor(e))) return e.preventDefault();
    if (e.key !== "Escape") return;
    e.preventDefault();
    close();
  });

  return {
    async open() {
      root.hidden = false;
      await look.load();
      openSet = await invoke<OpenSettings>("open_settings").catch(() => null);
      menuSet = await invoke<MenuLayout>("menu_layout").catch(() => null);
      agentSet = await invoke<SkiffAgentSettings>("get_skiff_agent").catch(() => null);
      agents = await invoke<AgentInfo[]>("list_agents").catch((e) => {
        error = String(e);
        return [];
      });
      render();
      if (tab === "appearance") root.querySelector<HTMLInputElement>(".tc-search")?.focus();
      else root.querySelector<HTMLButtonElement>(".set-agent .c-on:not(:disabled)")?.focus();
    },
    close,
    /** The native menu runs its accelerators here, not through the keydown above. */
    runKey: (a: Action) => !root.hidden && runKey(a),
    get isOpen() {
      return !root.hidden;
    },
  };
}
