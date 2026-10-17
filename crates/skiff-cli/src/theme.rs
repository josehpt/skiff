//! Theme commands: list themes and assign app, project, or session themes.

use anyhow::{Result, bail};
use skiff_client::Client;

use crate::{AppThemeCmd, ProjectThemeCmd, SessionThemeCmd, print, resolve};

/// Lists the theme catalogue available from this configuration folder.
pub fn ls(json: bool) -> Result<()> {
    let themes = skiff_core::theme::list();
    if json {
        return print::json(&themes);
    }
    let rows: Vec<Vec<String>> = themes
        .iter()
        .map(|t| vec![t.id.clone(), t.name.clone(), t.source.clone()])
        .collect();
    print::table(&["ID", "NAME", "SOURCE"], &rows);
    Ok(())
}

/// Sets the app-level default. It applies where neither a project nor a
/// session selects a theme.
pub fn app(cmd: AppThemeCmd) -> Result<()> {
    match cmd {
        AppThemeCmd::Set { theme } => skiff_core::config::set_app_theme(Some(&known(&theme)?)),
        AppThemeCmd::Clear => skiff_core::config::set_app_theme(None),
    }
}

/// Sets a project-level default. It applies where a session has no override.
pub fn project(cmd: ProjectThemeCmd) -> Result<()> {
    match cmd {
        ProjectThemeCmd::Set { project, theme } => {
            skiff_core::config::set_project_theme(&project, Some(&known(&theme)?))
        }
        ProjectThemeCmd::Clear { project } => skiff_core::config::set_project_theme(&project, None),
    }
}

/// Sets a session-level override in the daemon.
pub async fn session(c: &Client, cmd: SessionThemeCmd) -> Result<()> {
    let all = c.list_sessions().await?;
    match cmd {
        SessionThemeCmd::Set { session, theme } => {
            let id = resolve::session(&all, &session)?.id.clone();
            c.set_session_theme(&id, Some(known(&theme)?)).await?;
        }
        SessionThemeCmd::Clear { session } => {
            let id = resolve::session(&all, &session)?.id.clone();
            c.set_session_theme(&id, None).await?;
        }
    }
    Ok(())
}

/// A theme must exist in the same config folder the command uses. This keeps
/// a misspelled ID from silently becoming the default in the desktop app.
fn known(id: &str) -> Result<String> {
    let id = id.trim().to_string();
    if skiff_core::theme::list().iter().any(|t| t.id == id) {
        Ok(id)
    } else {
        bail!("no theme {id:?}; use `skiff theme ls`")
    }
}

#[cfg(test)]
mod tests {
    use super::known;

    #[test]
    fn accepts_a_builtin_id() {
        assert_eq!(known(" builtin:foot ").unwrap(), "builtin:foot");
    }

    #[test]
    fn rejects_an_unknown_id() {
        assert!(known("builtin:no-such-theme").is_err());
    }
}
