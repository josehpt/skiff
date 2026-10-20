/**
 * The keymap. The terminal owns Ctrl+letter; Skiff uses Ctrl+Shift+letter
 * (the Command key is its macOS equivalent). `[keys]` overrides any
 * action: `palette = "ctrl+shift+p"`.
 *
 * Primary-modifier shortcuts also use Shift. This leaves bare Ctrl sequences
 * for the terminal. Pane focus also uses Alt to keep Ctrl+Shift+Arrow for
 * terminal selection. `[keys]` can override a default when a platform needs it.
 *
 * Quit is the exception on macOS: Cmd+Shift+Q is the system's Log Out, so
 * Quit takes Cmd+Q, as every Mac app does.
 */

export type Action =
  | "palette" | "new-session" | "new-worktree" | "add-project" | "end-session"
  | "split-right" | "split-down" | "close-pane" | "next-waiting" | "back" | "rename" | "theme"
  | "copy" | "paste" | "find" | "font-bigger" | "font-smaller" | "font-reset"
  | "app-font-bigger" | "app-font-smaller" | "app-font-reset" | "settings" | "quit"
  | "focus-left" | "focus-right" | "focus-up" | "focus-down"
  | "project-menu" | "project-folder" | "project-copy-path" | "project-color" | "project-changes" | "project-files"
  | "region-next" | "region-prev" | "session-next" | "session-prev" | "list-project" | "list-back" | "list-main" | "shortcuts"
  | "focus-mode" | "maximize" | "mission-control" | "skiff-agent" | "ledger";

export const isMac = navigator.userAgent.includes("Macintosh");

export const DEFAULTS: Record<Action, string> = {
  palette: "ctrl+shift+p",
  "new-session": "ctrl+shift+t",
  "new-worktree": "ctrl+shift+n",
  "add-project": "ctrl+shift+o",
  "end-session": "ctrl+shift+k",
  "split-right": "ctrl+shift+d",
  "split-down": "ctrl+shift+s",
  "close-pane": "ctrl+shift+w",
  "next-waiting": "ctrl+shift+j",
  back: "ctrl+shift+b",
  rename: "ctrl+shift+r",
  theme: "ctrl+shift+x",
  copy: isMac ? "ctrl+c" : "ctrl+shift+c",
  paste: isMac ? "ctrl+v" : "ctrl+shift+v",
  find: "ctrl+shift+f",
  "font-bigger": "ctrl+shift+=",
  "font-smaller": "ctrl+shift+-",
  "font-reset": "ctrl+shift+0",
  "app-font-bigger": "ctrl+shift+alt+=",
  "app-font-smaller": "ctrl+shift+alt+-",
  "app-font-reset": "ctrl+shift+alt+0",
  settings: "ctrl+shift+,",
  quit: isMac ? "ctrl+q" : "ctrl+shift+q",
  "focus-left": "ctrl+shift+alt+left",
  "focus-right": "ctrl+shift+alt+right",
  "focus-up": "ctrl+shift+alt+up",
  "focus-down": "ctrl+shift+alt+down",
  "project-menu": "ctrl+shift+m",
  "project-folder": "ctrl+shift+z",
  "project-copy-path": "ctrl+shift+y",
  "project-color": "ctrl+shift+i",
  "project-changes": "ctrl+shift+g",
  "project-files": "ctrl+shift+h",
  "region-next": "f6",
  "region-prev": "shift+f6",
  "session-next": "ctrl+shift+down",
  "session-prev": "ctrl+shift+up",
  "list-project": "ctrl+shift+left",
  "list-back": "ctrl+shift+right",
  "list-main": "ctrl+shift+home",
  shortcuts: "f1",
  "focus-mode": "ctrl+shift+e",
  maximize: "ctrl+shift+enter",
  "mission-control": "ctrl+shift+a",
  "skiff-agent": "ctrl+shift+l",
  ledger: "ctrl+shift+alt+l",
};

interface Combo { ctrl: boolean; shift: boolean; alt: boolean; key: string }

const ALIASES: Record<string, string> = {
  left: "arrowleft", right: "arrowright", up: "arrowup", down: "arrowdown",
  plus: "=", "+": "=", minus: "-", comma: ",", esc: "escape", space: " ",
};

function parse(spec: string): Combo | null {
  const parts = spec.toLowerCase().split("+").map((p) => p.trim());
  // "ctrl++" splits into an empty last part: the key is "+".
  if (parts.at(-1) === "" && parts.length > 1) parts.splice(-2, 2, "+");
  const key = parts.pop();
  if (!key) return null;
  return {
    ctrl: parts.includes("ctrl"),
    shift: parts.includes("shift"),
    alt: parts.includes("alt"),
    key: ALIASES[key] ?? key,
  };
}

/** The key a keydown means, independent of Shift and keyboard layout for letters and digits. */
function eventKey(e: KeyboardEvent): string {
  const m = /^(Key|Digit)(.)$/.exec(e.code);
  if (m) return m[2].toLowerCase();
  if (e.code === "Equal") return "=";
  if (e.code === "Minus") return "-";
  if (e.code === "Comma") return ",";
  return e.key.toLowerCase();
}

let table: [Action, Combo][] = [];
let labels: Record<string, string> = {};
let combos: Record<string, Combo> = {};

/** Ctrl in configuration means the platform's primary application modifier. */
function primaryHeld(e: KeyboardEvent): boolean {
  return isMac ? e.metaKey : e.ctrlKey;
}

/** Builds the keymap from the defaults and `[keys]` overrides. */
export function setKeymap(overrides: Record<string, string>) {
  const specs: Record<string, string> = { ...DEFAULTS };
  for (const [k, v] of Object.entries(overrides)) if (k in DEFAULTS) specs[k] = v;
  table = [];
  labels = {};
  combos = {};
  for (const [action, spec] of Object.entries(specs) as [Action, string][]) {
    const c = parse(spec);
    if (!c) continue;
    table.push([action, c]);
    labels[action] = label(c);
    combos[action] = c;
  }
}
setKeymap({});

/** The action a keydown triggers, if any. */
export function actionFor(e: KeyboardEvent): Action | null {
  const key = eventKey(e);
  for (const [action, c] of table) {
    if (c.ctrl === primaryHeld(e) && c.shift === e.shiftKey && c.alt === e.altKey && c.key === key) return action;
  }
  return null;
}

function label(c: Combo): string {
  const k = c.key.startsWith("arrow") ? c.key.slice(5)[0].toUpperCase() + c.key.slice(6) : c.key.length === 1 || /^f\d+$/.test(c.key) ? c.key.toUpperCase() : c.key[0].toUpperCase() + c.key.slice(1);
  return [c.ctrl && (isMac ? "⌘" : "Ctrl"), c.alt && "Alt", c.shift && "Shift", k].filter(Boolean).join("+");
}

/** A keydown spelled as keyLabel spells a key, so a menu can match its hints. */
export function eventLabel(e: KeyboardEvent): string {
  return label({ ctrl: primaryHeld(e), shift: e.shiftKey, alt: e.altKey, key: eventKey(e) });
}

/** "Ctrl+Shift+T" (or "⌘+Shift+T"), for menus and the palette. */
export function keyLabel(action: Action): string {
  return labels[action] ?? "";
}

const MENU_KEYS: Record<string, string> = { "=": "Equal", "-": "Minus", ",": "Comma", " ": "Space", escape: "Escape", enter: "Enter", home: "Home", end: "End" };

/** The action's key as the menu bar spells it ("Ctrl+Shift+T"), or "" for none. */
export function menuAccel(action: Action): string {
  const c = combos[action];
  if (!c) return "";
  const k = c.key.startsWith("arrow") ? "Arrow" + c.key[5].toUpperCase() + c.key.slice(6) : MENU_KEYS[c.key] ?? c.key.toUpperCase();
  return [c.ctrl && "CmdOrCtrl", c.alt && "Alt", c.shift && "Shift", k].filter(Boolean).join("+");
}

/** The Skiff agent's name from Settings. Its F1 line reads it. */
let skiffAgentName = "Skipper";
export const setSkiffAgentName = (name: string) => void (skiffAgentName = name);

/** A DESCRIBE line with the names Settings sets filled in. */
export const describeText = (what: string) => what.replaceAll("{skiff-agent}", skiffAgentName);

/** What each action does, for the shortcut sheet. Grouped as the menu bar is. */
export const DESCRIBE: [string, Action, string][] = [
  ["File", "new-session", "New session in the highlighted worktree, as its + does"],
  ["File", "new-worktree", "New worktree"],
  ["File", "add-project", "Add project"],
  ["File", "settings", "Settings"],
  ["File", "quit", "Quit Skiff. Sessions keep running in skiffd"],
  ["Edit", "copy", "Copy the terminal selection. In Settings, the selected text in the field"],
  ["Edit", "paste", "Paste into the terminal. In Settings, into the field. A one-line field gets one line"],
  ["Edit", "find", "Find in the terminal"],
  ["Edit", "rename", "Rename the session, or the group or session row in the list"],
  ["Edit", "theme", "Session theme, or the theme of the group or session row in the list. On a project or another row, the project theme"],
  ["Edit", "end-session", "End the highlighted session, or the focused pane's session. On a group row, kill all its sessions. It asks first"],
  ["Project", "project-menu", "Menu for the highlighted row: project, worktree, group, session, Changes or Files"],
  ["Project", "project-folder", "Show the project in the file manager. On a file or change row, that file"],
  ["Project", "project-copy-path", "Copy the project path. On a file or change row, that file's path"],
  ["Project", "project-color", "Project color"],
  ["Project", "project-changes", "Show or hide Changes"],
  ["Project", "project-files", "Show or hide Files"],
  ["View", "palette", "Command palette. In mission control, search the sessions on the board"],
  ["View", "split-right", "Add pane right"],
  ["View", "split-down", "Add pane below"],
  ["View", "close-pane", "Take the session out of its group: the focused pane, or the session row in the list. It stays selected. On a group row, ungroup. Sessions keep running"],
  ["View", "next-waiting", "Next waiting session"],
  ["View", "skiff-agent", "{skiff-agent}, the Skiff agent: show its pane. With none running, start it in its folder"],
  ["View", "ledger", "Show or hide the Ledger beside {skiff-agent}'s pane. Only while its pane is the one pane on screen. Focus mode keeps the Ledger, maximize hides it"],
  ["View", "back", "Back to the last session"],
  ["View", "session-next", "Next row in the list: worktree, group, session, Changes or Files. A change or file shows its preview, which takes the keys. Changes, Files and folders keep the keys: Space opens or closes them. In Settings, the next section. In mission control, the next screen in the lane"],
  ["View", "session-prev", "Previous row in the list. In Settings, the previous section. In mission control, the previous screen in the lane"],
  ["View", "list-project", "To the current project's row in the list. The panes stay. In Settings, to the section tabs. In mission control, the lane to the left"],
  ["View", "list-back", "From a project or fleet row back to the session on screen. In Settings, into the section. In mission control, the lane to the right"],
  ["View", "list-main", "To the project's main worktree in the list, from any row"],
  ["View", "focus-left", "Focus the pane to the left"],
  ["View", "focus-right", "Focus the pane to the right"],
  ["View", "focus-up", "Focus the pane above"],
  ["View", "focus-down", "Focus the pane below"],
  ["View", "focus-mode", "Focus mode: hide the list and bars. On a session or group row, not a worktree, Changes or Files. From maximize it shows the view's panes again. Ctrl+Shift+Up/Down step through them. Going to another view ends it. With one pane it is maximize, and either key leaves it. On {skiff-agent}'s pane it keeps the Ledger, and maximize from it hides the Ledger. In mission control, only the running sessions (the default) or all"],
  ["View", "maximize", "Maximize the focused pane, and hide the rest. On a session row only. Ctrl+Shift+Up/Down maximize the next pane of the view. Press again to go back, to focus mode if it came from there. In mission control, the lit session; pressed again, back to mission control"],
  ["View", "mission-control", "Mission control: every session's screen, one lane per fleet. Arrows or Ctrl+Shift+Arrows pick a screen: Up/Down in the lane, Left/Right in the lane beside it. Click or Enter goes to a session. Press again or Esc to close. With text in the search, the first Esc clears it"],
  ["View", "font-bigger", "Bigger text. On a session, its pane only: a terminal with the keys, or a session row"],
  ["View", "font-smaller", "Smaller text. On a session, its pane only"],
  ["View", "font-reset", "Reset text size. On a session, its pane goes back to the app size"],
  ["View", "app-font-bigger", "Bigger app text: the app and every terminal"],
  ["View", "app-font-smaller", "Smaller app text"],
  ["View", "app-font-reset", "Reset app text size"],
  ["Help", "region-next", "Next region: session list, terminal"],
  ["Help", "region-prev", "Previous region"],
  ["Help", "shortcuts", "This sheet"],
];
