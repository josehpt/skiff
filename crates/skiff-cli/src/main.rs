//! `skiff`: drive skiffd from a shell. Sessions, groups, agents and projects.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use skiff_client::Client;
use skiff_core::{group::SplitDir, session::Role};
use layout::Side;

mod attach;
mod callsign;
mod daemon;
mod events;
mod groups;
mod info;
mod layout;
mod ledger;
mod open_with;
mod peek;
mod print;
mod resolve;
mod sessions;
mod skipper;
mod theme;
mod wait;
mod watch;

#[derive(Parser)]
#[command(
    name = "skiff",
    version,
    about = "Command-line client for skiffd",
    after_help = "A session is a full id, a unique id prefix, or the name the sidebar shows. A group is an id, prefix or name.\nInside a Skiff pane, `.` is this pane's session, or the group that holds it."
)]
struct Cli {
    /// The skiffd socket. Default: $SKIFF_SOCKET, else the daemon's own default.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show this skiff's version and path, and the daemon's version,
    /// protocol, pid and socket. Warns when another skiff comes first on
    /// PATH, or skiffd's version is not the one this skiff was built for.
    Status,
    /// Manage the daemon itself.
    #[command(subcommand)]
    Daemon(DaemonCmd),
    /// List sessions.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Start a session and print its id.
    #[command(
        after_help = "--type starts a task type from Settings › Skiff agent › Task types ([skiff_agent.tasks] in projects.toml): its agent, with its model flag. With no --name, the session takes the type's name, numbered when one runs (review, review 2).\nWith no row for the type, it exits 1 and starts nothing.\n--model replaces the row's model for this session. A custom agent takes its model in its command, so it refuses --model.\n`skiff agents` lists the types with the command each one starts."
    )]
    New {
        /// The session's name.
        #[arg(long)]
        name: Option<String>,
        /// Working directory. Default: the current directory.
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,
        /// Start this agent, as the app's + menu does.
        #[arg(long, value_name = "ID", conflicts_with = "command")]
        agent: Option<String>,
        /// Start this task type's agent with its model. Not with --agent.
        #[arg(long = "type", value_name = "TYPE", conflicts_with_all = ["agent", "command"])]
        task_type: Option<String>,
        /// With --type: this model in place of the type's model.
        #[arg(long, value_name = "MODEL", requires = "task_type")]
        model: Option<String>,
        /// Default: agent with --agent or --type, else shell.
        #[arg(long, value_enum)]
        role: Option<RoleArg>,
        /// Put the new session into this group.
        #[arg(long, value_name = "GROUP")]
        group: Option<String>,
        /// With --group: the side of the focused pane it goes on.
        #[arg(long, value_enum, requires = "group")]
        split: Option<Side>,
        /// Print the new session as JSON.
        #[arg(long)]
        json: bool,
        /// Program and arguments, after `--`. Default: the login shell.
        #[arg(last = true, value_name = "CMD")]
        command: Vec<String>,
    },
    /// Type text into an idle session. `-` reads the text from stdin. Put
    /// text that starts with `-` after `--`.
    #[command(
        after_help = "send checks the session before it types any bytes. Exit codes:\n  0  sent\n  1  error\n  3  the session is waiting (approval prompt or bell). A person answers it in the pane. No flag overrides this.\n  4  the session is working. Skiff does not type into a busy session.\n  5  a shell or an unknown program is in front. Use --shell only to type a shell command. A program started with `skiff new -- CMD` or a custom agent shows its launch shell, so a send to it needs --shell.\n  6  the session is done\n  7  the session is your own ($SKIFF_SESSION)\n  8  --enter went to a known agent, but no turn started in 5 seconds. The text can be in its input box. Run `skiff peek` on the session.\n  9  a known agent shows text in its input box or its busy line, or the session took input after the check. Nothing was typed. A second send after exit 8 gets this code while the text is still in the box."
    )]
    Send {
        session: String,
        #[arg(required = true)]
        text: Vec<String>,
        /// Press Enter after the text.
        #[arg(long)]
        enter: bool,
        /// Send the bytes as they are. Default: newlines become Enter.
        #[arg(long)]
        raw: bool,
        /// Permit an idle session with a shell or an unknown program in
        /// front. It does not override any other refusal.
        #[arg(long)]
        shell: bool,
    },
    /// Rename a session. An empty name clears it.
    Rename { session: String, name: String },
    /// Run a session in this terminal. Ctrl-\ detaches. The session
    /// takes this terminal's size, in the app too, and gets its own size
    /// back on detach. A quick second Ctrl-C is held back once.
    Attach { session: String },
    /// Attach to the Skiff agent (Skipper). Start it first if it does not
    /// run. Ctrl-\ detaches; the agent keeps running.
    #[command(
        after_help = "Reads [skiff_agent] in projects.toml: the folder, the name, the agent and whether it is on.\nWith no Skiff agent session, it sets up the folder (missing files come back, existing ones stay), starts the agent, and starts `skiff watch` for it. One watcher runs per daemon.\nA Skiff agent session that is a bare shell (after a restore, or after its agent quit) gets the agent again: its last conversation when Settings continues on restore, else a new one.\nWith no skiffd running, it starts one, as the app does.\nThe app and this command find the same session: skiffd marks it, and two starts at once give one.\nAttach gives the session this terminal's size, in the app too, until you detach. Then the pane gets its own size back.\n--move-to resolves a relative PATH against this shell's folder and stores the full path. The old folder keeps every file and gets MOVED.md; Skiff never deletes it. The conversation does not move. Skipper's session ends first, and tasks that still run stop the change unless you confirm."
    )]
    Skipper {
        /// Do not attach. Print the session id.
        #[arg(long)]
        no_attach: bool,
        /// Change the Skiff agent's folder to PATH, as Settings › Skiff agent ›
        /// Folder does: copy, check and switch to a new or empty folder, or
        /// switch to a Skiff folder. It shows the preview and asks first.
        #[arg(long, value_name = "PATH", conflicts_with = "no_attach")]
        move_to: Option<std::path::PathBuf>,
        /// With --move-to: set up a new ledger in place of a copy.
        #[arg(long, requires = "move_to")]
        empty: bool,
        /// With --move-to: show the preview and change nothing.
        #[arg(long, requires = "move_to")]
        dry_run: bool,
    },
    /// Print a session's screen as plain text.
    Peek {
        session: String,
        /// The last N lines of history and screen. Default: the screen.
        #[arg(long, short = 'n', value_name = "N")]
        lines: Option<usize>,
    },
    /// Wait until sessions reach a state. Exit 0 when all do, 1 on timeout,
    /// 2 when one closes or exits first, 3 when one is waiting.
    #[command(
        after_help = "Exit codes:\n  0  every session reached the state. With --any: one session reached it. Its id, name and state print on stdout.\n  1  timeout\n  2  a session closed or exited before it reached the state. With --any: every session did.\n  3  a session is waiting (approval prompt or bell). A person answers it in its pane.\nWith --for idle, done or any-not-working, wait stops at once when any session is waiting. --for waiting is not changed.\nWith --for idle, a known agent that is quiet but shows its busy line (\"esc to interrupt\") is not idle yet.\nIn a Skiff pane ($SKIFF_SESSION), or with no terminal on stdout, the timeout defaults to 45 seconds and is at most 60.\nAfter exit 0, read the screen with `skiff peek`: idle does not prove that an agent finished."
    )]
    Wait {
        #[arg(required = true)]
        sessions: Vec<String>,
        #[arg(long = "for", value_enum, value_name = "STATE")]
        target: wait::Target,
        /// Stop when the first session reaches the state.
        #[arg(long)]
        any: bool,
        /// Give up after this many seconds. For an agent: default 45, at most 60.
        #[arg(long, value_name = "SECS")]
        timeout: Option<f64>,
    },
    /// Print each state change, exit and removal as one line, for every
    /// session or for the named ones. Runs until Ctrl+C or --timeout.
    #[command(
        after_help = "Lines: a state change prints the new state, an exit prints `exit` and the code, a closed session prints `removed`. No output or screen text prints.\nWith --json, each line is one JSON object:\n  {\"event\":\"state\",\"session\":ID,\"name\":NAME,\"state\":\"idle\"}\n  {\"event\":\"exit\",\"session\":ID,\"name\":NAME,\"code\":0}\n  {\"event\":\"removed\",\"session\":ID,\"name\":NAME}\nExit codes:\n  0  --timeout ran out, or every named session was removed\n  1  error\n  2  a name matches no session\nRun it in the background so your own turn stays free. idle means quiet, not done. After any line, read the screen with `skiff peek`."
    )]
    Events {
        /// Default: every session, also the ones started later.
        sessions: Vec<String>,
        /// One JSON object for each line.
        #[arg(long)]
        json: bool,
        /// Stop after this many seconds.
        #[arg(long, value_name = "SECS")]
        timeout: Option<f64>,
    },
    /// Follow the open tasks in the Skiff agent's ledger for that agent.
    /// Skiff starts it with the agent's session; it stops when that session
    /// ends. One runs per daemon.
    #[command(
        after_help = "A pane in an open ledger row (sent, pending, blocked) that:\n  waits (approval or bell): the person gets a desktop notification. The agent gets nothing.\n  ends a turn, goes idle after a prompt, exits or is gone: the agent gets one line, typed while it is idle and its input box is empty. Lines that wait for it are joined.\n  is idle with a pending row: the line names the oldest pending uid and its `skiff ledger send` command, once per idle stretch. The row is read again just before the line is typed, and the line is dropped if the task left pending.\n  restarted since the send: the agent gets one line that names the task as interrupted, once per run.\nskiffd types a line only if the person typed nothing in the agent's pane since the check. If no turn starts, the log says undelivered and Enter is not pressed again.\nA pane stays unread until the agent or the person marks it seen, so a new run tells it again, as it does a pending row.\nIt logs one line for each thing it does."
    )]
    Watch {
        /// The Skiff agent's session.
        skipper: String,
        /// Do not send desktop notifications.
        #[arg(long)]
        no_notify: bool,
    },
    /// Mark a session seen, as focusing its pane does.
    Seen { session: String },
    /// Stop sessions.
    Kill {
        #[arg(required = true)]
        sessions: Vec<String>,
    },
    /// Named splits of sessions.
    #[command(subcommand, visible_alias = "g")]
    Group(GroupCmd),
    /// List the agents skiffd knows.
    Agents {
        #[arg(long)]
        json: bool,
    },
    /// List the projects in projects.toml.
    Projects {
        #[arg(long)]
        json: bool,
    },
    /// List and assign terminal color themes.
    #[command(after_help = "A session theme overrides its project's theme. A project theme overrides the app theme. Clear returns to the next setting.")]
    Theme {
        #[command(subcommand)]
        cmd: ThemeCmd,
    },
    /// Show and set the command that opens a diff or a file.
    #[command(
        name = "open-with",
        after_help = "Keys: diff, text, markdown, html. `{path}` in a command becomes the file; without it the file goes at the end. `{target}` in the diff command becomes what to compare.\nA terminal tool shows in the peek over the panes. An app with its own window just starts. A file with no command opens in the system's default app.\nChanges apply to the next file or diff the app opens."
    )]
    OpenWith {
        #[command(subcommand)]
        cmd: OpenWithCmd,
    },
    /// The Skiff agent's task ledger: ledger.db in its folder.
    #[command(after_help = "States: sent, pending (not delivered: the pane was working), blocked (the pane needs the user), done.\nls and show print interrupted for a sent or blocked task whose pane restarted since the send: a restore runs a new shell under the old pane id. They print gone for an open task whose pane no longer exists. A plan with tasks shows done when every task is done, else sent.\nID is the row number or the 8-character uid that add prints. Neither is ever reused.\nResults: a task writes to results/<date>-<uid>-<slug>.md next to the ledger; a plan owns results/<date>-<uid>-<slug>/ with plan.md, and its tasks write <uid>-<slug>.md in that folder. add stores and prints the path and makes no file. rm keeps the files.\nThe ledger is ~/skiff-agent/ledger.db, or ledger.db in the [skiff_agent] folder override. SKIFF_LEDGER overrides the path.\nOn first use, an old ledger.md next to it is imported and kept as ledger.md.bak.")]
    Ledger {
        #[command(subcommand)]
        cmd: LedgerCmd,
    },
}

#[derive(Subcommand)]
pub enum LedgerCmd {
    /// Add a task, or a plan with --plan. Prints its id, uid and result path.
    Add {
        /// The pane the task went to: an id, a unique prefix or a name. It
        /// must exist. A plan has no pane.
        #[arg(long, required_unless_present = "plan", conflicts_with = "plan")]
        pane: Option<String>,
        #[arg(long)]
        task: String,
        /// A plan: a row with no pane that groups tasks under it.
        #[arg(long)]
        plan: bool,
        /// The plan this task belongs to: its id or uid.
        #[arg(long, value_name = "ID", conflicts_with = "plan")]
        parent: Option<String>,
        /// Default: the project that holds the pane's folder.
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "sent")]
        state: String,
        /// Where the child writes its result. Default: a path in results/.
        #[arg(long, value_name = "PATH")]
        result: Option<String>,
        /// Free text that names the work elsewhere: a URL, a ticket key,
        /// anything. Skiff stores it and never reads it.
        #[arg(long = "ref", value_name = "TEXT")]
        reference: Option<String>,
        /// The task type the task went out as, from `skiff agents`. Skiff
        /// stores it and never acts on it.
        #[arg(long = "type", value_name = "TYPE")]
        task_type: Option<String>,
        /// Add the task as pending and send it at once, as `ledger send` does.
        #[arg(long, conflicts_with_all = ["plan", "state"])]
        send: bool,
        /// With --send: press Enter after the text.
        #[arg(long, requires = "send")]
        enter: bool,
        /// With --send: as `send --shell`.
        #[arg(long, requires = "send")]
        shell: bool,
    },
    /// Send a pending or interrupted task to its pane, then mark it sent.
    /// Exits with `send`'s code.
    #[command(
        after_help = "Only a pending or interrupted task with a live pane is sent. Else it exits 1 and sends nothing.\nThe text defaults to the task and its result path. `-` reads the text from stdin.\nIt runs every `send` check. On exit 0 the task is sent. On exit 8 (no turn started) it is blocked: the text can be in the input box. On a refusal it keeps its state, and `skiff watch` names it again when the pane is idle."
    )]
    Send {
        id: String,
        /// Default: the task and its result path.
        text: Vec<String>,
        /// Press Enter after the text.
        #[arg(long)]
        enter: bool,
        /// As `send --shell`.
        #[arg(long)]
        shell: bool,
    },
    /// Change a task. Only the given fields change. A new pane, or the state
    /// sent, records the pane's current generation.
    Update {
        id: String,
        #[arg(long)]
        state: Option<String>,
        #[arg(long, value_name = "PATH")]
        result: Option<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        pane: Option<String>,
        #[arg(long)]
        project: Option<String>,
        /// Move the task under this plan: its id or uid.
        #[arg(long, value_name = "ID")]
        parent: Option<String>,
        #[arg(long = "ref", value_name = "TEXT")]
        reference: Option<String>,
        #[arg(long = "type", value_name = "TYPE")]
        task_type: Option<String>,
    },
    /// List tasks, oldest first, each plan's tasks under it. LIVE says if
    /// the pane still exists.
    Ls {
        /// sent, pending, blocked, done, interrupted or gone.
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one task.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Take a task out of the ledger. Its result file stays.
    Rm { id: String },
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Move skiffd onto a new binary and keep every session, with its
    /// process, screen and scrollback. Checks afterwards that each one survived.
    Reload {
        /// The skiffd to run. Default: the running daemon's executable as it
        /// is on disk now, for example after a rebuild.
        #[arg(long, value_name = "PATH")]
        binary: Option<PathBuf>,
    },
    /// Stop skiffd. It saves the layout and ends every session. From a
    /// pane, this is the way to stop it: skiffd ignores SIGTERM and SIGINT
    /// from its own sessions.
    Stop,
}

#[derive(Subcommand)]
enum GroupCmd {
    /// List groups with their panes. `*` marks the focused pane.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Make a group and print its id. With no sessions it holds one empty pane.
    New {
        /// Default: its agents' names, as the app names a group.
        #[arg(long)]
        name: Option<String>,
        sessions: Vec<String>,
        /// Put every pane in one line. Default: the app's shapes.
        #[arg(long, value_enum)]
        dir: Option<DirArg>,
        /// The group's folder, where its empty panes start sessions.
        #[arg(long, value_name = "DIR")]
        cwd: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Put a session into a group. It fills an empty pane, else splits the
    /// focused pane. The session leaves any other group.
    Add {
        group: String,
        session: String,
        /// The side of the target pane it goes on. Default: right.
        #[arg(long, value_enum)]
        split: Option<Side>,
        /// Split this pane instead of the focused one.
        #[arg(long, value_name = "SESSION")]
        of: Option<String>,
    },
    /// Take a session out of a group. The session keeps running.
    Rm { group: String, session: String },
    /// Rename a group.
    Rename { group: String, name: String },
    /// Delete groups. Their sessions keep running.
    Delete {
        #[arg(required = true)]
        groups: Vec<String>,
    },
}

#[derive(Subcommand)]
enum OpenWithCmd {
    /// List each key and its command.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Set a key's command.
    Set {
        #[arg(value_enum)]
        key: OpenKey,
        /// The command, quoted as one argument.
        command: String,
    },
    /// Clear a key. The default applies.
    Clear {
        #[arg(value_enum)]
        key: OpenKey,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum OpenKey {
    Diff,
    Text,
    Markdown,
    Html,
}

impl OpenKey {
    fn name(self) -> &'static str {
        match self {
            OpenKey::Diff => "diff",
            OpenKey::Text => "text",
            OpenKey::Markdown => "markdown",
            OpenKey::Html => "html",
        }
    }
}

#[derive(Subcommand)]
enum ThemeCmd {
    /// List available themes.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Set or clear the app theme.
    App {
        #[command(subcommand)]
        cmd: AppThemeCmd,
    },
    /// Set or clear a project's theme.
    Project {
        #[command(subcommand)]
        cmd: ProjectThemeCmd,
    },
    /// Set or clear a session's theme override.
    #[command(visible_alias = "terminal")]
    Session {
        #[command(subcommand)]
        cmd: SessionThemeCmd,
    },
}

#[derive(Subcommand)]
enum AppThemeCmd {
    /// Set the app theme.
    Set { theme: String },
    /// Clear the app theme. The default applies.
    Clear,
}

#[derive(Subcommand)]
enum ProjectThemeCmd {
    /// Set the project theme.
    Set { project: String, theme: String },
    /// Clear the project theme. The app theme applies.
    Clear { project: String },
}

#[derive(Subcommand)]
enum SessionThemeCmd {
    /// Set the session theme override.
    Set { session: String, theme: String },
    /// Clear the session theme override. The project or app theme applies.
    Clear { session: String },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum DirArg {
    /// Side by side.
    Row,
    /// Stacked.
    Col,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum RoleArg {
    Agent,
    Task,
    Server,
    Shell,
}

impl From<RoleArg> for Role {
    fn from(r: RoleArg) -> Self {
        match r {
            RoleArg::Agent => Role::Agent,
            RoleArg::Task => Role::Task,
            RoleArg::Server => Role::Server,
            RoleArg::Shell => Role::Shell,
        }
    }
}

fn main() {
    // Rust ignores SIGPIPE, so `skiff ls | head` would panic on a closed pipe.
    // The default action ends the process quietly, like other CLI tools.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(run(cli)) {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("skiff: {e:#}");
            std::process::exit(1);
        }
    }
}

/// The exit code. Only `wait`, `events` and `send` return other than 0.
async fn run(cli: Cli) -> Result<i32> {
    let socket = cli.socket.unwrap_or_else(skiff_core::socket::socket_path);
    match cli.cmd {
        // Read locally: no daemon needed.
        Cmd::Projects { json } => return info::projects(json).map(|_| 0),
        Cmd::Theme { cmd: ThemeCmd::Ls { json } } => return theme::ls(json).map(|_| 0),
        Cmd::Theme { cmd: ThemeCmd::App { cmd } } => return theme::app(cmd).map(|_| 0),
        Cmd::Theme { cmd: ThemeCmd::Project { cmd } } => return theme::project(cmd).map(|_| 0),
        Cmd::OpenWith { cmd } => return open_with::run(cmd).map(|_| 0),
        Cmd::Ledger { cmd } => return ledger::run(&socket, cmd).await,
        Cmd::Status => return info::status(&socket).await.map(|_| 0),
        Cmd::Daemon(DaemonCmd::Reload { binary }) => return daemon::reload(&socket, binary).await.map(|_| 0),
        Cmd::Daemon(DaemonCmd::Stop) => return daemon::stop(&socket).await.map(|_| 0),
        // Starts skiffd when it does not run.
        Cmd::Skipper { move_to: Some(to), empty, dry_run, .. } => return skipper::move_to(&socket, &to, empty, dry_run).await.map(|_| 0),
        Cmd::Skipper { no_attach, .. } => return skipper::run(&socket, no_attach).await.map(|_| 0),
        // It connects again after a daemon reload, so it holds no connection here.
        Cmd::Watch { skipper, no_notify } => {
            return watch::watch(&socket, watch::Options { skipper, notify: !no_notify }).await
        }
        _ => {}
    }
    let c = connect(&socket).await?;
    if let Cmd::Wait { sessions, target, any, timeout } = &cli.cmd {
        let t = wait::timeout_for(*timeout, wait::agent_caller());
        if timeout.is_some_and(|asked| t != Some(asked)) {
            eprintln!("skiff: --timeout capped at {}s for an agent", t.unwrap_or_default());
        }
        return wait::wait(&c, sessions, *target, *any, t).await;
    }
    if let Cmd::Events { sessions, json, timeout } = &cli.cmd {
        return events::events(&c, sessions, *json, *timeout).await;
    }
    if let Cmd::Send { session, text, enter, raw, shell } = &cli.cmd {
        let o = sessions::Send { text, enter: *enter, raw: *raw, shell: *shell };
        return sessions::send(&c, session, o).await;
    }
    match cli.cmd {
        Cmd::Ls { json } => sessions::ls(&c, json).await,
        Cmd::New { name, cwd, agent, task_type, model, role, group, split, json, command } => {
            let opts = sessions::New { name, cwd, agent, task_type, model, role: role.map(Role::from), command };
            sessions::new(&c, opts, group.as_deref(), split, json).await
        }
        Cmd::Group(g) => group(&c, g).await,
        Cmd::Rename { session, name } => sessions::rename(&c, &session, &name).await,
        Cmd::Kill { sessions: ids } => sessions::kill(&c, &ids).await,
        Cmd::Attach { session } => attach::attach(&c, &session).await,
        Cmd::Peek { session, lines } => peek::peek(&c, &session, lines).await,
        Cmd::Seen { session } => sessions::seen(&c, &session).await,
        Cmd::Agents { json } => info::agents(&c, json).await,
        Cmd::Theme { cmd: ThemeCmd::Session { cmd } } => theme::session(&c, cmd).await,
        Cmd::Status | Cmd::Daemon(_) | Cmd::Projects { .. } | Cmd::Theme { .. } | Cmd::OpenWith { .. } | Cmd::Ledger { .. } | Cmd::Wait { .. } | Cmd::Events { .. } | Cmd::Watch { .. } | Cmd::Send { .. } | Cmd::Skipper { .. } => {
            unreachable!()
        }
    }
    .map(|_| 0)
}

async fn group(c: &Client, cmd: GroupCmd) -> Result<()> {
    match cmd {
        GroupCmd::Ls { json } => groups::ls(c, json).await,
        GroupCmd::New { name, sessions, dir, cwd, json } => {
            let dir = dir.map(|d| match d {
                DirArg::Row => SplitDir::Row,
                DirArg::Col => SplitDir::Col,
            });
            groups::new(c, groups::NewGroup { name, sessions, dir, cwd }, json).await
        }
        GroupCmd::Add { group, session, split, of } => {
            groups::add(c, &group, &session, groups::Place { split, of }).await
        }
        GroupCmd::Rm { group, session } => groups::rm(c, &group, &session).await,
        GroupCmd::Rename { group, name } => groups::rename(c, &group, &name).await,
        GroupCmd::Delete { groups: ids } => groups::delete(c, &ids).await,
    }
}

/// Connects, with a plain message when no daemon listens.
pub async fn connect(socket: &std::path::Path) -> Result<Client> {
    if !socket.exists() {
        anyhow::bail!("skiffd is not running (no socket at {})", socket.display());
    }
    Client::connect(socket)
        .await
        .with_context(|| format!("skiffd is not running at {}", socket.display()))
}
