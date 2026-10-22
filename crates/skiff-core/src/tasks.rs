//! Task types: `[skiff_agent.tasks]` names the agent and the model that each
//! kind of work starts with. Skipper reads the task and picks the type. Skiff
//! stores the table and builds the command line. It reads no task text.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{config::TaskTypes, project::AgentInfo, shell::shell_quote};

/// Where a person sets the table, for error messages.
pub const SETTINGS: &str = "Settings › Skiff agent › Task types";

/// The model flag of each known agent, from its `--help` (2026-10-15:
/// claude 2.1.301, codex 0.162.1, grok 1.0.44). An agent with no flag here
/// takes its model in its command, as a custom agent does. Keep in step with
/// `MODEL_FLAGS` in apps/desktop/src/appearance/taskTypes.ts.
pub fn model_flag(agent: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("--model"),
        "codex" | "grok" => Some("-m"),
        _ => None,
    }
}

/// A task type name: lowercase letters, digits and `-`, starting with a
/// letter or a digit. It is the TOML key and the `--type` value.
pub fn check_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("A task type needs a name.");
    }
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !first_ok || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!("Use lowercase letters, digits and -, as in code-review. {name:?} is not a task type name.");
    }
    Ok(name)
}

/// A model name: one word. Empty is `None`, the agent's default.
pub fn check_model(model: Option<&str>) -> Result<Option<&str>> {
    let Some(m) = model.map(str::trim).filter(|m| !m.is_empty()) else { return Ok(None) };
    if m.chars().any(char::is_whitespace) {
        bail!("A model is one word, as in opus. {m:?} has spaces.");
    }
    Ok(Some(m))
}

/// How a known agent takes an instruction file at start, from its CLI docs
/// (2026-10-22). Each one adds to the agent's own prompt, so the project's
/// `AGENTS.md` / `CLAUDE.md` still applies. Keep in step with
/// `INSTRUCTION_FLAGS` in apps/desktop/src/appearance/taskTypes.ts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Delivery {
    /// The flag takes the file's path: claude `--append-system-prompt-file`.
    File(&'static str),
    /// codex `-c developer_instructions=<text>`: the config key takes text,
    /// not a path, so Skiff reads the file when the session starts.
    CodexDeveloperInstructions,
}

/// The way `agent` takes an instruction file. An agent with none refuses:
/// Skiff does not send the file as a message or write it anywhere.
pub fn delivery(agent: &AgentInfo) -> Result<Delivery> {
    if agent.custom {
        bail!("{} is a custom agent. Skiff does not know how it takes an instruction file. Put the flag in its command in Settings › Agents.", agent.id);
    }
    match agent.id.as_str() {
        "claude" => Ok(Delivery::File("--append-system-prompt-file")),
        "codex" => Ok(Delivery::CodexDeveloperInstructions),
        id => bail!("{id} has no flag Skiff knows for an instruction file. Use claude or codex for this type, or take the file out."),
    }
}

/// The largest file codex gets on its command line. argv has a limit.
pub const MAX_INLINE: u64 = 64 * 1024;

/// An instruction file as the row has it: one line, a full path or `~/…`.
/// Empty is `None`. The file need not be there yet.
pub fn check_instructions(path: Option<&str>) -> Result<Option<&str>> {
    let Some(p) = path.map(str::trim).filter(|p| !p.is_empty()) else { return Ok(None) };
    if p.contains(['\n', '\r']) || !(p.starts_with('/') || p.starts_with("~/")) {
        bail!("Name the instruction file with a full path or ~/, as in ~/.skiff/types/review.md.");
    }
    Ok(Some(p))
}

/// The row's file as a full path, with `~` expanded. It must be a file.
pub fn instructions_path(path: &str) -> Result<PathBuf> {
    let Some(p) = check_instructions(Some(path))? else { bail!("No instruction file.") };
    let full = crate::config::expand_home(Path::new(p));
    if !full.is_file() {
        bail!("The instruction file {p} is not there. Make it, or change the path in {SETTINGS}.");
    }
    Ok(full)
}

/// The words that give `agent` the file at `path` (a full path).
pub fn instruction_args(agent: &AgentInfo, path: &Path) -> Result<Vec<String>> {
    match delivery(agent)? {
        Delivery::File(flag) => Ok(vec![flag.to_string(), path.display().to_string()]),
        Delivery::CodexDeveloperInstructions => {
            let size = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?.len();
            if size > MAX_INLINE {
                bail!("The instruction file {} has {size} bytes. codex takes it on its command line, so keep it under {MAX_INLINE}.", path.display());
            }
            let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            // codex reads the value as TOML: a quoted string stays text, whatever the file holds.
            Ok(vec!["-c".to_string(), format!("developer_instructions={}", toml::Value::String(text))])
        }
    }
}

/// The instruction words as Settings and `skiff agents` show them: the path
/// as typed, and for codex a stand-in for the text it reads at start.
pub fn instruction_preview(agent: &AgentInfo, path: &str) -> Result<String> {
    let quote = |s: &str| if s.chars().all(|c| c.is_ascii_alphanumeric() || "/~._-+".contains(c)) { s.to_string() } else { shell_quote(s) };
    Ok(match delivery(agent)? {
        Delivery::File(flag) => format!("{flag} {}", quote(path)),
        Delivery::CodexDeveloperInstructions => format!("-c developer_instructions=<text of {path}>"),
    })
}

/// The agent's command with the model flag at the end, as `continue_line`
/// adds `--continue`. Flags the person put in the command stay. Without a
/// model, the plain command. An agent with no model flag refuses a model.
pub fn command(agent: &AgentInfo, model: Option<&str>) -> Result<String> {
    let cmd = agent.command.trim();
    let Some(model) = check_model(model)? else { return Ok(cmd.to_string()) };
    match model_flag(&agent.id).filter(|_| !agent.custom) {
        Some(flag) => Ok(format!("{cmd} {flag} {model}")),
        None => bail!("{} takes its model in its command. Change the command in Settings › Agents.", agent.id),
    }
}

/// One task type as Settings and `skiff agents --json` show it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct TaskType {
    #[serde(rename = "type")]
    pub name: String,
    pub agent: String,
    /// `None`: the agent's default. Always `None` for an agent that takes its
    /// model in its command.
    pub model: Option<String>,
    /// The instruction file as the row has it. `None`: none.
    pub instructions: Option<String>,
    /// The command line the type starts. `None` when the agent is gone, or
    /// when it cannot take the instruction file or the file is not there.
    pub command: Option<String>,
    /// Why the row does not start what it says.
    pub error: Option<String>,
}

/// Every row with its built command, in file order.
pub fn resolve(rows: &TaskTypes, agents: &[AgentInfo]) -> Vec<TaskType> {
    rows.0
        .iter()
        .map(|(name, row)| {
            let mut t = TaskType { name: name.clone(), agent: row.agent.clone(), model: None, instructions: row.instructions.clone(), command: None, error: None };
            let Some(a) = agents.iter().find(|a| a.id == row.agent) else {
                t.error = Some(format!("There is no agent {}. Pick another agent in {SETTINGS}.", row.agent));
                return t;
            };
            let takes_model = !a.custom && model_flag(&a.id).is_some();
            match check_model(row.model.as_deref()) {
                Ok(m) if takes_model => t.model = m.map(str::to_string),
                Ok(None) => {}
                Ok(Some(_)) => t.error = Some(format!("{} takes its model in its command. The model here is not used.", a.id)),
                Err(e) => {
                    t.error = Some(e.to_string());
                    return t;
                }
            }
            t.command = command(a, t.model.as_deref()).ok();
            // A file the agent cannot take, or that is not there, starts nothing: the row says why.
            if let Some(p) = &t.instructions {
                match instruction_preview(a, p).and_then(|w| instructions_path(p).map(|_| w)) {
                    Ok(w) => t.command = t.command.map(|c| format!("{c} {w}")),
                    Err(e) => {
                        t.command = None;
                        t.error = Some(e.to_string());
                    }
                }
            }
            t
        })
        .collect()
}

/// What `skiff new --type` starts: the row's agent and the words of its
/// command line, the instruction file's last. A `model` replaces the row's
/// model for this session. A file the agent cannot take, or that is not
/// there, starts nothing.
pub fn start<'a>(rows: &TaskTypes, agents: &'a [AgentInfo], name: &str, model: Option<&str>) -> Result<(&'a AgentInfo, Vec<String>)> {
    let Some(row) = rows.get(name) else {
        let known: Vec<&str> = rows.0.iter().map(|(k, _)| k.as_str()).collect();
        let list = if known.is_empty() { "There are none.".to_string() } else { format!("Known: {}.", known.join(", ")) };
        bail!("There is no task type {name}. {list} Add it in {SETTINGS}.");
    };
    let Some(a) = agents.iter().find(|a| a.id == row.agent) else {
        bail!("Task type {name} starts agent {}, and there is no such agent. Pick another agent in {SETTINGS}.", row.agent);
    };
    let takes_model = !a.custom && model_flag(&a.id).is_some();
    // A model in the row of an agent that takes none is not used; a model the person names is refused.
    let model = match check_model(model)? {
        Some(m) => Some(m),
        None if takes_model => check_model(row.model.as_deref())?,
        None => None,
    };
    // The command splits on spaces, as before. The file's words do not: a path may have spaces.
    let mut words: Vec<String> = command(a, model)?.split_whitespace().map(String::from).collect();
    if let Some(p) = &row.instructions {
        let args = delivery(a).and_then(|_| instructions_path(p)).and_then(|full| instruction_args(a, &full));
        words.extend(args.map_err(|e| anyhow::anyhow!("Task type {name}: {e}"))?);
    }
    Ok((a, words))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TaskRow;

    fn agent(id: &str, command: &str, custom: bool) -> AgentInfo {
        AgentInfo {
            id: id.into(),
            command: command.into(),
            default_command: if custom { String::new() } else { id.into() },
            custom,
            installed: true,
            enabled: true,
        }
    }

    fn rows(list: &[(&str, &str, Option<&str>)]) -> TaskTypes {
        TaskTypes(list.iter().map(|(k, a, m)| (k.to_string(), TaskRow { agent: a.to_string(), model: m.map(str::to_string), instructions: None })).collect())
    }

    /// A folder with `review.md` in it, and its path. The name may have a space.
    fn file(tag: &str, text: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("skiff-type-file {tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("review.md");
        std::fs::write(&f, text).unwrap();
        (dir, f)
    }

    fn with_file(mut r: TaskTypes, name: &str, path: &str) -> TaskTypes {
        r.0.iter_mut().find(|(k, _)| k == name).unwrap().1.instructions = Some(path.into());
        r
    }

    #[test]
    fn instruction_files_are_full_paths() {
        assert_eq!(check_instructions(None).unwrap(), None);
        assert_eq!(check_instructions(Some("  ")).unwrap(), None);
        assert_eq!(check_instructions(Some(" ~/.skiff/types/review.md ")).unwrap(), Some("~/.skiff/types/review.md"));
        assert_eq!(check_instructions(Some("/etc/x.md")).unwrap(), Some("/etc/x.md"));
        for bad in ["review.md", "./review.md", "~review.md", "/a\nb"] {
            assert!(check_instructions(Some(bad)).is_err(), "{bad:?}");
        }
        assert!(instructions_path("/no/such/skiff/file.md").unwrap_err().to_string().contains("is not there"));
    }

    #[test]
    fn each_agent_takes_the_file_its_own_way() {
        let (dir, f) = file("args", "Review only. Say \"done\".\nkey = 1\n");
        let claude = agent("claude", "claude", false);
        assert_eq!(instruction_args(&claude, &f).unwrap(), ["--append-system-prompt-file".to_string(), f.display().to_string()]);
        // codex: the text, quoted as a TOML string, so a line like `key = 1` stays text.
        let codex = instruction_args(&agent("codex", "codex", false), &f).unwrap();
        assert_eq!(codex[0], "-c");
        let value = codex[1].strip_prefix("developer_instructions=").unwrap();
        let parsed: toml::Value = toml::from_str(&format!("v = {value}")).unwrap();
        assert_eq!(parsed["v"].as_str(), Some("Review only. Say \"done\".\nkey = 1\n"));
        // No known way: a refusal, never a write or a message.
        for a in [agent("grok", "grok", false), agent("gemini", "agy", false), agent("opencode", "opencode", false), agent("claude", "claude", true)] {
            assert!(instruction_args(&a, &f).is_err(), "{}", a.id);
        }
        assert_eq!(instruction_preview(&claude, "~/types/my review.md").unwrap(), "--append-system-prompt-file '~/types/my review.md'");
        assert_eq!(instruction_preview(&agent("codex", "codex", false), "~/r.md").unwrap(), "-c developer_instructions=<text of ~/r.md>");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn codex_refuses_a_file_too_long_for_its_command_line() {
        let (dir, f) = file("long", &"x".repeat(MAX_INLINE as usize + 1));
        assert!(instruction_args(&agent("codex", "codex", false), &f).unwrap_err().to_string().contains("keep it under"));
        // claude reads the file itself.
        assert!(instruction_args(&agent("claude", "claude", false), &f).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn start_adds_the_file_after_the_model() {
        let (dir, f) = file("start", "Review only.");
        let p = f.display().to_string();
        let agents = [agent("claude", "claude --dangerously-skip-permissions", false), agent("codex", "codex", false), agent("grok", "grok", false)];
        let r = rows(&[("review", "claude", Some("opus")), ("impl", "codex", None), ("fast", "grok", None), ("gone", "claude", None)]);
        let r = with_file(with_file(with_file(with_file(r, "review", &p), "impl", &p), "fast", &p), "gone", "/no/such/skiff/file.md");
        // The path is one word, spaces and all.
        assert_eq!(start(&r, &agents, "review", None).unwrap().1, ["claude", "--dangerously-skip-permissions", "--model", "opus", "--append-system-prompt-file", p.as_str()]);
        assert_eq!(start(&r, &agents, "review", Some("sonnet")).unwrap().1[3], "sonnet");
        assert_eq!(start(&r, &agents, "impl", None).unwrap().1, ["codex", "-c", "developer_instructions=\"Review only.\""]);
        let e = start(&r, &agents, "fast", None).unwrap_err().to_string();
        assert!(e.starts_with("Task type fast: grok has no flag"), "{e}");
        let e = start(&r, &agents, "gone", None).unwrap_err().to_string();
        assert!(e.contains("is not there") && e.contains(SETTINGS), "{e}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn resolve_shows_the_file_in_the_command_or_why_not() {
        let (dir, f) = file("resolve", "Review only.");
        let p = f.display().to_string();
        let agents = [agent("claude", "claude", false), agent("aider", "aider", true)];
        let r = rows(&[("review", "claude", Some("opus")), ("docs", "aider", None), ("gone", "claude", None)]);
        let r = with_file(with_file(with_file(r, "review", &p), "docs", &p), "gone", "/no/such/skiff/file.md");
        let t = resolve(&r, &agents);
        assert_eq!(t[0].instructions.as_deref(), Some(p.as_str()));
        assert_eq!(t[0].command.as_deref(), Some(format!("claude --model opus --append-system-prompt-file '{p}'").as_str()));
        assert!(t[0].error.is_none());
        assert!(t[1].command.is_none() && t[1].error.as_deref().unwrap().contains("custom agent"), "{:?}", t[1]);
        assert!(t[2].command.is_none() && t[2].error.as_deref().unwrap().contains("is not there"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_are_slugs() {
        for ok in ["review", "code-review", "t2", "2fa"] {
            assert_eq!(check_name(ok).unwrap(), ok);
        }
        assert_eq!(check_name(" spec ").unwrap(), "spec");
        for bad in ["", "Review", "-x", "a b", "a_b", "ünï", "a.b"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn commands_add_the_flag_at_the_end() {
        let claude = agent("claude", "claude --dangerously-skip-permissions", false);
        assert_eq!(command(&claude, Some("opus")).unwrap(), "claude --dangerously-skip-permissions --model opus");
        assert_eq!(command(&claude, Some("  ")).unwrap(), "claude --dangerously-skip-permissions");
        assert_eq!(command(&agent("codex", "codex", false), Some("gpt-5-codex")).unwrap(), "codex -m gpt-5-codex");
        assert_eq!(command(&agent("grok", "grok", false), Some("grok-4")).unwrap(), "grok -m grok-4");
        assert!(command(&agent("aider", "aider --model sonnet", true), Some("x")).is_err());
        assert!(command(&agent("gemini", "agy", false), Some("x")).is_err());
        assert!(command(&claude, Some("two words")).is_err());
    }

    #[test]
    fn resolves_each_row_in_order() {
        let agents = [agent("claude", "claude", false), agent("aider", "aider --model sonnet", true)];
        let t = resolve(&rows(&[("spec", "claude", Some("opus")), ("docs", "aider", Some("x")), ("test", "claude", None), ("old", "gone", None)]), &agents);
        assert_eq!(t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["spec", "docs", "test", "old"]);
        assert_eq!(t[0].command.as_deref(), Some("claude --model opus"));
        assert_eq!((t[1].model.as_deref(), t[1].command.as_deref()), (None, Some("aider --model sonnet")));
        assert!(t[1].error.is_some());
        assert_eq!((t[2].command.as_deref(), t[2].error.as_deref()), (Some("claude"), None));
        assert!(t[3].command.is_none() && t[3].error.as_deref().unwrap().contains("no agent gone"));
    }

    #[test]
    fn start_takes_the_row_and_a_named_model_wins() {
        let agents = [agent("claude", "claude", false), agent("aider", "aider", true)];
        let r = rows(&[("review", "claude", Some("opus")), ("plain", "claude", None), ("docs", "aider", Some("x"))]);
        assert_eq!(start(&r, &agents, "review", None).unwrap().1.join(" "), "claude --model opus");
        assert_eq!(start(&r, &agents, "review", Some("sonnet")).unwrap().1.join(" "), "claude --model sonnet");
        assert_eq!(start(&r, &agents, "plain", None).unwrap().1.join(" "), "claude");
        assert_eq!(start(&r, &agents, "docs", None).unwrap().1.join(" "), "aider");
        assert!(start(&r, &agents, "docs", Some("y")).is_err());
        let e = start(&r, &agents, "nope", None).unwrap_err().to_string();
        assert!(e.contains("no task type nope") && e.contains(SETTINGS) && e.contains("review, plain, docs"), "{e}");
    }
}
