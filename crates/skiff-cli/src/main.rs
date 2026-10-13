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
    /// Show the daemon's version, protocol, pid and socket.
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
        /// Default: agent with --agent, else shell.
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
        after_help = "send checks the session before it types any bytes. Exit codes:\n  0  sent\n  1  error\n  3  the session is waiting (approval prompt or bell). A person answers it in the pane. No flag overrides this.\n  4  the session is working. Skiff does not type into a busy session.\n  5  a shell or an unknown program is in front. Use --shell only to type a shell command. A program started with `skiff new -- CMD` or a custom agent shows its launch shell, so a send to it needs --shell.\n  6  the session is done\n  7  the session is your own ($SKIFF_SESSION)\n  8  --enter went to a known agent, but no turn started in 5 seconds. The text can be in its input box. Run `skiff peek` on the session. Do not send it again."
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
    /// takes this terminal's size, in the app too.
    Attach { session: String },
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
        after_help = "Exit codes:\n  0  every session reached the state. With --any: one session reached it. Its id, name and state print on stdout.\n  1  timeout\n  2  a session closed or exited before it reached the state. With --any: every session did.\n  3  a session is waiting (approval prompt or bell). A person answers it in its pane.\nWith --for idle, done or any-not-working, wait stops at once when any session is waiting. --for waiting is not changed.\nGive each wait a --timeout. After exit 0, read the screen with `skiff peek`: idle does not prove that an agent finished."
    )]
    Wait {
        #[arg(required = true)]
        sessions: Vec<String>,
        #[arg(long = "for", value_enum, value_name = "STATE")]
        target: wait::Target,
        /// Stop when the first session reaches the state.
        #[arg(long)]
        any: bool,
        /// Give up after this many seconds.
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
        after_help = "A pane in an open ledger row (sent, pending, blocked) that:\n  waits (approval or bell): the person gets a desktop notification. The agent gets nothing.\n  goes idle after work, exits or is gone: the agent gets one line, typed while it is idle and its input box is empty. Lines that wait for it are joined.\nA pane stays unread until the agent or the person marks it seen, so a new run tells it again.\nIt logs one line for each thing it does."
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
    #[command(after_help = "States: sent, pending (not delivered: the pane was working), blocked (the pane needs the user), done.\nThe ledger is ~/skiff-agent/ledger.db, or ledger.db in the [skiff_agent] folder override. SKIFF_LEDGER overrides the path.\nOn first use, an old ledger.md next to it is imported and kept as ledger.md.bak.")]
    Ledger {
        #[command(subcommand)]
        cmd: LedgerCmd,
    },
}

#[derive(Subcommand)]
pub enum LedgerCmd {
    /// Add a task. Prints its id.
    Add {
        /// The pane the task went to: an id, a unique prefix or a name. It must exist.
        #[arg(long)]
        pane: String,
        #[arg(long)]
        task: String,
        /// Default: the project that holds the pane's folder.
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "sent")]
        state: String,
        /// Where the child writes a long result.
        #[arg(long, value_name = "PATH")]
        result: Option<String>,
    },
    /// Change a task. Only the given fields change.
    Update {
        id: i64,
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
    },
    /// List tasks, oldest first. LIVE says if the pane still exists.
    Ls {
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show one task.
    Show {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Take a task out of the ledger.
    Rm { id: i64 },
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
        Cmd::Ledger { cmd } => return ledger::run(&socket, cmd).await.map(|_| 0),
        Cmd::Status => return info::status(&socket).await.map(|_| 0),
        Cmd::Daemon(DaemonCmd::Reload { binary }) => return daemon::reload(&socket, binary).await.map(|_| 0),
        // It connects again after a daemon reload, so it holds no connection here.
        Cmd::Watch { skipper, no_notify } => {
            return watch::watch(&socket, watch::Options { skipper, notify: !no_notify }).await
        }
        _ => {}
    }
    let c = connect(&socket).await?;
    if let Cmd::Wait { sessions, target, any, timeout } = &cli.cmd {
        return wait::wait(&c, sessions, *target, *any, *timeout).await;
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
        Cmd::New { name, cwd, agent, role, group, split, json, command } => {
            let opts = sessions::New { name, cwd, agent, role: role.map(Role::from), command };
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
        Cmd::Status | Cmd::Daemon(_) | Cmd::Projects { .. } | Cmd::Theme { .. } | Cmd::OpenWith { .. } | Cmd::Ledger { .. } | Cmd::Wait { .. } | Cmd::Events { .. } | Cmd::Watch { .. } | Cmd::Send { .. } => {
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
