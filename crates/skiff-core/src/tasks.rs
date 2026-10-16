//! Task types: `[skiff_agent.tasks]` names the agent and the model that each
//! kind of work starts with. Skipper reads the task and picks the type. Skiff
//! stores the table and builds the command line. It reads no task text.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::{config::TaskTypes, project::AgentInfo};

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
    /// The command line the type starts. `None` when the agent is gone.
    pub command: Option<String>,
    /// Why the row does not start what it says.
    pub error: Option<String>,
}

/// Every row with its built command, in file order.
pub fn resolve(rows: &TaskTypes, agents: &[AgentInfo]) -> Vec<TaskType> {
    rows.0
        .iter()
        .map(|(name, row)| {
            let mut t = TaskType { name: name.clone(), agent: row.agent.clone(), model: None, command: None, error: None };
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
            t
        })
        .collect()
}

/// What `skiff new --type` starts: the row's agent and its command line. A
/// `model` replaces the row's model for this session.
pub fn start<'a>(rows: &TaskTypes, agents: &'a [AgentInfo], name: &str, model: Option<&str>) -> Result<(&'a AgentInfo, String)> {
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
    Ok((a, command(a, model)?))
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
        TaskTypes(list.iter().map(|(k, a, m)| (k.to_string(), TaskRow { agent: a.to_string(), model: m.map(str::to_string) })).collect())
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
        assert_eq!(start(&r, &agents, "review", None).unwrap().1, "claude --model opus");
        assert_eq!(start(&r, &agents, "review", Some("sonnet")).unwrap().1, "claude --model sonnet");
        assert_eq!(start(&r, &agents, "plain", None).unwrap().1, "claude");
        assert_eq!(start(&r, &agents, "docs", None).unwrap().1, "aider");
        assert!(start(&r, &agents, "docs", Some("y")).is_err());
        let e = start(&r, &agents, "nope", None).unwrap_err().to_string();
        assert!(e.contains("no task type nope") && e.contains(SETTINGS) && e.contains("review, plain, docs"), "{e}");
    }
}
