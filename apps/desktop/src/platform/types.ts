// Wire types from skiff-core, as the Tauri commands return them.

export type SessionState = "working" | "waiting" | "idle" | "done";
export type Role = "agent" | "task" | "server" | "shell";

export interface SessionInfo {
  id: string;
  label: string;
  role: Role;
  cwd: string;
  command: string;
  /** Program in the PTY's foreground process group. Absent from older daemons. */
  foreground_program?: string | null;
  args: string[];
  state: SessionState;
  cols: number;
  rows: number;
  pid: number | null;
  exit_code: number | null;
  /** This terminal's own theme id; wins over the project's. */
  theme: string | null;
  /** A name Skiff or the user gave; wins over `title`. */
  name: string | null;
  /** Terminal title the program set, if any. */
  title: string | null;
  /** Unix ms. */
  started_at: number;
  /** Unix ms. */
  last_output_at: number;
  /** Other worktrees its processes ran in during the last minute. Absent from an older daemon. */
  away?: Away[];
  /** The last agent seen in front, and its title. Absent from an older daemon. */
  was?: Was | null;
  /** `was` as a restore found it, until an agent runs again. Absent from an older daemon. */
  resume?: Was | null;
  /** An agent's turn or a shell command ended, and no pane with the keys has shown it since. Absent from an older daemon. */
  unread?: boolean;
  /** Exit code of the last agent the pane ran, until a pane with the keys shows it. Absent from an older daemon. */
  agent_exit?: number | null;
  /** A tool in the app's peek. Not a session to the user. Absent from older daemons. */
  peek?: boolean;
  /** A peek command took the terminal: a terminal tool. */
  interactive?: boolean;
  /** Counts the input the session took. A guarded submit names it. */
  input_gen?: number;
  /** The Skiff agent's session. skiffd marks at most one live session. */
  coordinator?: boolean;
  /** The `skiff watch` processes registered for this session. Absent when none is, or from a daemon older than protocol 27. */
  watch?: WatchInfo | null;
}

/** The watchers registered for a session. */
export interface WatchInfo {
  /** Unix ms of the oldest live registration. */
  since: number;
  pids: number[];
  /** The newest failure a watcher reports. */
  failure?: WatchFailure | null;
  /** The live panes with an open ledger task that the watchers follow. Absent: unknown (an older watcher or daemon, or no ledger read yet). */
  follows?: string[] | null;
}

export interface WatchFailure {
  /** "undelivered": a line was typed and no turn started. "ledger": the ledger could not be read. */
  kind: string;
  /** The watch log line. */
  message: string;
  /** Unix ms. */
  at: number;
}

/** An agent that ran in a session, and the last title it set. */
export interface Was {
  agent: string;
  title: string | null;
}

/** A worktree other than the session's own, where one of its processes ran. */
export interface Away {
  /** The worktree root. */
  path: string;
  /** The last process seen there. */
  command: string;
  /** Unix ms. */
  at: number;
}

export interface Worktree {
  path: string;
  /** Short branch name. `null` when detached. */
  branch: string | null;
  head: string;
  is_main: boolean;
  locked: boolean;
  prunable: boolean;
}

export interface Project {
  name: string;
  short: string;
  path: string;
  color: string | null;
  agents: string[];
  /** Project icon as a data URL, configured or found in the repo. */
  icon: string | null;
  /** Absolute path of the background image behind the terminals, when set. */
  background: string | null;
  /** Pane opacity over the background image, in percent. Absent: the default. */
  background_opacity?: number | null;
  worktrees: Worktree[];
  /** The branch the main checkout normally holds. `null` when unknown or from an older skiffd. */
  default_branch?: string | null;
  /** Path missing, not a git repo, and so on. `worktrees` is empty then. */
  error: string | null;
  /** Out of the tree until it opens again. Its worktrees are not read. Absent from an older daemon. */
  closed?: boolean;
  /** The fleet it sits in. Absent: under Projects, in no fleet. */
  fleet?: string | null;
  /** `changes = false` in the Host's projects.toml: no Changes section. Absent: shown. */
  changes?: boolean | null;
  /** The project's own theme id. Absent: the app theme. */
  theme?: string | null;
  /** Shows a Files tree under each worktree. */
  files?: boolean;
}

/** A diff as plain text from the selected Host's git (`git_diff@1`). */
export interface HostDiff {
  /** Unified diff text. Empty: no change. */
  text: string;
  /** Longer than the Host's limit: `text` is its start, cut at a line end. */
  truncated: boolean;
  /** A binary file: `text` holds git's note only. */
  binary: boolean;
  /** An untracked file, compared with an empty file. */
  untracked: boolean;
}

export type DaemonEvent =
  | { event: "state"; session: string; state: SessionState }
  | { event: "exit"; session: string; code: number | null }
  | { event: "session_created"; session: SessionInfo }
  | { event: "session_removed"; session: string }
  | { event: "session_updated"; session: SessionInfo }
  | { event: "turn_ended"; session: string }
  | { event: "groups_changed" }
  | { event: "title"; session: string; title: string | null }
  | { event: "away"; session: string; away: Away[] };

export interface AgentInfo {
  id: string;
  /** What the + menu runs: the user's command, else the default. */
  command: string;
  /** Empty for a custom agent. */
  default_command: string;
  /** Added by the user. Absent from an older daemon. */
  custom?: boolean;
  installed: boolean;
  enabled: boolean;
}

/** `[appearance]` in the daemon config. `font_size` is absent from an older daemon. */
export interface Appearance {
  theme: string | null;
  font_size?: number | null;
  /** Mission control's filter: `running` or `all`. Absent: `running`. */
  mission_filter?: string | null;
}

export interface DaemonStatus {
  connected: boolean;
  version: string | null;
  /** The daemon's protocol. `null` when the app could not ask. Absent from an older app backend. */
  protocol?: number | null;
  socket: string;
  /** The daemon's pid, when the app could read it. */
  pid: number | null;
  spawned: boolean;
  /** The daemon differs from this app and was not replaced. */
  warning: DaemonWarning | null;
  /** The version of an older daemon this app replaced or reloaded. */
  replaced: string | null;
  /** The daemon is older and can reload onto the app's skiffd. */
  reload: boolean;
}

/** What `reload_daemon` did. */
export interface Reloaded {
  status: DaemonStatus;
  /** Why the reload failed or is not proven. `null`: it worked. */
  error: string | null;
  /** The app talks to a new connection: the page subscribes again. */
  reconnected: boolean;
  /** Live sessions that did not come through, by name. */
  missing: string[];
  /** Sessions whose input, typed during the reload, was dropped. */
  lost: string[];
}

export interface DaemonWarning {
  /** `remote`: a remote Host this app reaches but never restarts or updates.
   *  `identity`: that Host answered with another Skiff identity. */
  kind: "outdated" | "protocol" | "newer" | "hung" | "failed" | "remote" | "identity";
  message: string;
  /** Live sessions a restart stops. `null` when the daemon did not say. */
  sessions: number | null;
}

export interface FolderInfo {
  /** Git top level. `null` when the folder is not in a repository. */
  root: string | null;
  name: string;
  short: string;
  color: string;
  branch: string | null;
  worktrees: number;
  /** The icon the Host found in the repo, as a data URL. */
  icon: string | null;
  /** Why the folder cannot be added. */
  error: string | null;
}

export type SplitDir = "row" | "col";

/** `row`: a left, b right. `col`: a top, b bottom. `ratio` is the share of `a`. */
export type Layout =
  | { type: "pane"; session: string }
  | { type: "split"; dir: SplitDir; ratio: number; a: Layout; b: Layout };

/** A named split kept by the daemon. `id` is empty until the daemon assigns one. */
export interface Group {
  id: string;
  name: string;
  layout: Layout;
  focus: string | null;
  /** Where the group's empty panes start their sessions. */
  cwd?: string | null;
}

export type ThemeColorFamily = "Neutral" | "Beige" | "Red" | "Orange" | "Yellow" | "Green" | "Cyan" | "Blue" | "Purple" | "Pink";

export interface TerminalTheme {
  /** builtin:<slug> or file:<name>. */
  id: string;
  name: string;
  source: "built-in" | "file";
  foreground: string;
  background: string;
  cursor: string | null;
  cursor_foreground?: string;
  selection: string | null;
  selection_foreground?: string;
  color_family?: ThemeColorFamily;
  main_color?: string;
  /** ANSI 0–15. */
  palette: string[];
  /** App colors from the theme file (Superset's ui block), when it has them. */
  ui?: Record<string, string>;
}
