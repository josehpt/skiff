use std::path::PathBuf;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub keys: Keys,
    #[serde(default)]
    pub agents: Agents,
    #[serde(default)]
    pub appearance: Appearance,
    #[serde(default)]
    pub open: Open,
    #[serde(default)]
    pub skiff_agent: SkiffAgent,
    #[serde(default, rename = "project")]
    pub projects: Vec<ProjectConfig>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Appearance {
    /// The theme id the app and every terminal without its own theme use.
    /// Absent: Harbor.
    pub theme: Option<String>,
    /// Terminal text size in points; the app text scales with it. Absent: 13.
    pub font_size: Option<u8>,
    /// Linux: `header-bar` or `menu-bar`. Absent: the header bar on GNOME,
    /// else the menu bar. The app reads it when the window opens.
    pub menu_layout: Option<String>,
}

/// Which apps Skiff hands things to.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Open {
    /// File kinds that open on selection in the peek.
    #[serde(default)]
    pub peek: Vec<String>,
    /// Shows a changed file's diff. `{target}` becomes what to compare.
    /// Absent: `git diff --color=always`.
    pub diff: Option<String>,
    /// Opens text files. `{path}` becomes the file. Absent: the OS default app.
    pub text: Option<String>,
    /// Opens `.md` files. Absent: the OS default app.
    pub markdown: Option<String>,
    /// Opens `.html` files. Absent: the OS default app.
    pub html: Option<String>,
}

/// `[skiff_agent]`: the one agent session that coordinates the others. The
/// app reads and writes it; the daemon passes it by.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct SkiffAgent {
    /// The label on its row, its pane and its key. Absent: Skipper.
    pub name: Option<String>,
    /// The agent id it starts. Absent: the first enabled agent.
    pub agent: Option<String>,
    /// Its folder. Absent: no Skiff agent row.
    pub folder: Option<PathBuf>,
    /// A restored pane continues the last conversation in the folder.
    #[serde(default)]
    pub continue_on_restore: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Agents {
    /// Agent ids the + menu offers. Absent: every installed agent.
    pub enabled: Option<Vec<String>>,
    /// Command lines that replace the defaults, by agent id.
    #[serde(default)]
    pub commands: std::collections::BTreeMap<String, String>,
    /// Default agents the user took out. Restore brings them back.
    #[serde(default)]
    pub removed: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Theme {
    /// A CSS file with custom properties. Relative to the config directory.
    pub file: Option<String>,
}

/// Key overrides by action: `palette = "ctrl+shift+p"`. The app holds the
/// defaults and the action names; unknown actions are ignored.
pub type Keys = std::collections::BTreeMap<String, String>;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    #[default]
    Stacked,
    SideBySide,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub cmd: String,
    pub port_env: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ProjectConfig {
    pub name: String,
    /// Rail label, 3 characters. Derived from `name` when absent.
    pub short: Option<String>,
    pub path: PathBuf,
    pub color: Option<String>,
    /// Image for the rail: a path, relative to the project or absolute.
    /// Absent: detect one in the repo. Empty: no icon.
    pub icon: Option<String>,
    /// Image behind the terminals: a path, relative to the project or absolute.
    pub background: Option<String>,
    /// How opaque the panes are over the background image, in percent. Absent: 85.
    pub background_opacity: Option<u8>,
    pub editor: Option<String>,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub layout: Layout,
    pub server: Option<ServerConfig>,
    /// Shows a Files tree under each worktree. The app reads and writes it;
    /// the daemon passes it by.
    #[serde(default)]
    pub files: bool,
    /// `false` hides the Changes section and the +/- counts. Absent: shown.
    pub changes: Option<bool>,
    /// Theme id for this project's chrome and its terminals. Absent: the app theme.
    pub theme: Option<String>,
    /// Closed: kept with its settings, but out of the rail until it opens again.
    #[serde(default)]
    pub closed: bool,
    /// The fleet this project sits in on the rail. Absent: on the rail by itself.
    pub fleet: Option<String>,
}

impl ProjectConfig {
    pub fn short(&self) -> String {
        self.short.clone().unwrap_or_else(|| short_name(&self.name))
    }
}

/// The default rail label: the first 3 letters or digits, lower case.
pub fn short_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(3)
        .collect::<String>()
        .to_lowercase()
}

/// What Skiff would add for `dir`, before anything is written.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct FolderInfo {
    /// The git top level. `None` when `dir` is not in a repository.
    pub root: Option<PathBuf>,
    pub name: String,
    pub short: String,
    pub color: String,
    pub branch: Option<String>,
    pub worktrees: usize,
    /// The icon Skiff found in the repo, as a data URL, and its path
    /// relative to the root.
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub icon_path: Option<String>,
    /// Why the folder cannot be added.
    pub error: Option<String>,
}
