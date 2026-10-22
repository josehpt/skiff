//! Group commands: ls, new, add, rm, rename, delete. Edits follow the app:
//! a session sits in one group at most, four panes at most, and a group
//! never goes empty: it keeps a slot and a folder.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use skiff_client::Client;
use skiff_core::{
    group::{is_slot, Group, Layout, SplitDir},
    session::SessionInfo,
};

use crate::{
    layout::{self, Side, MAX_PANES},
    print, resolve, sessions,
};

/// The app's `autoName`: agent names joined with " + ", three at most.
fn auto_name(ids: &[String], all: &[SessionInfo]) -> String {
    let names: Vec<String> = ids
        .iter()
        .map(|id| all.iter().find(|s| &s.id == id).map_or("session".into(), resolve::agent_name))
        .collect();
    let mut out = names.iter().take(3).cloned().collect::<Vec<_>>().join(" + ");
    if names.len() > 3 {
        out.push_str(&format!(" +{}", names.len() - 3));
    }
    out
}

/// Renames a group named after a template, after its layout changed from
/// `before`, as the app's `syncTemplateName` and `placeInSlot` do.
fn follow_name(g: &mut Group, before: &Layout, all: &[SessionInfo]) {
    let was = layout::filled(before);
    let now = layout::filled(&g.layout);
    let preset = layout::PRESETS.contains(&g.name.as_str());
    // As the app's `placeInSlot`: a template group takes its agents' names
    // when one of its empty panes fills.
    let filled_slot = preset && was.len() < now.len() && before.sessions().iter().any(|s| is_slot(s));
    if filled_slot {
        g.name = auto_name(&now, all);
    } else if preset {
        if let Some(shape) = layout::shape_name(&g.layout) {
            g.name = shape.to_string();
        }
    }
}

pub async fn ls(c: &Client, json: bool) -> Result<()> {
    let groups = c.list_groups().await?;
    if json {
        return print::json(&groups);
    }
    let all = c.list_sessions().await?;
    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let mut head = format!("{}  {}", print::short_id(&g.id), g.name);
        if let Some(cwd) = &g.cwd {
            head.push_str(&format!("  {}", print::tilde(std::path::Path::new(cwd))));
        }
        println!("{head}");
        let mut rows = Vec::new();
        tree(&g.layout, g.focus.as_deref(), &all, 1, &mut rows);
        print::table(&["", "", "", ""], &rows);
    }
    Ok(())
}

/// One row per split and pane, indented by depth. `*` marks the focus.
fn tree(l: &Layout, focus: Option<&str>, all: &[SessionInfo], depth: usize, rows: &mut Vec<Vec<String>>) {
    let pad = "  ".repeat(depth);
    match l {
        Layout::Split { dir, ratio, a, b } => {
            let d = match dir {
                SplitDir::Row => "row",
                SplitDir::Col => "col",
            };
            rows.push(vec![format!("{pad}{d} {:.0}%", ratio * 100.0), String::new(), String::new(), String::new()]);
            tree(a, focus, all, depth + 1, rows);
            tree(b, focus, all, depth + 1, rows);
        }
        Layout::Pane { session } => {
            let mark = if focus == Some(session.as_str()) { "*" } else { "" };
            let row = if is_slot(session) {
                vec![format!("{pad}{session}"), "(empty)".into(), String::new(), mark.into()]
            } else if let Some(s) = all.iter().find(|s| &s.id == session) {
                vec![
                    format!("{pad}{}", print::short_id(session)),
                    resolve::display_name(s).into(),
                    sessions::state(s.state).into(),
                    mark.into(),
                ]
            } else {
                vec![format!("{pad}{}", print::short_id(session)), "(gone)".into(), String::new(), mark.into()]
            };
            rows.push(row);
        }
    }
}

/// An absolute folder that exists.
fn folder(cwd: Option<PathBuf>) -> Result<String> {
    let here = std::env::current_dir().context("current directory")?;
    let dir = cwd.map_or(here.clone(), |d| here.join(d));
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    Ok(dir.canonicalize().unwrap_or(dir).to_string_lossy().into_owned())
}

/// Ids for session arguments, each once.
fn ids(all: &[SessionInfo], args: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for a in args {
        let id = resolve::session(all, a)?.id.clone();
        if out.contains(&id) {
            bail!("session {} is named twice", print::short_id(&id));
        }
        out.push(id);
    }
    Ok(out)
}

/// Takes `ids` out of every group but `keep`, as the app does when a session
/// moves. A group that loses its last session keeps a slot and a folder.
async fn release(c: &Client, groups: &[Group], keep: &str, ids: &[String], all: &[SessionInfo]) -> Result<()> {
    for g in groups.iter().filter(|g| g.id != keep) {
        let mut next = g.clone();
        for id in ids {
            if !next.layout.sessions().contains(id) {
                continue;
            }
            next.layout = layout::remove(&next.layout, id);
            if next.cwd.is_none() && layout::filled(&next.layout).is_empty() {
                next.cwd = all.iter().find(|s| &s.id == id).map(|s| s.cwd.to_string_lossy().into_owned());
            }
        }
        if next.layout != g.layout {
            fix_focus(&mut next);
            follow_name(&mut next, &g.layout, all);
            c.save_group(next).await?;
            eprintln!("skiff: moved out of group {}", g.name);
        }
    }
    Ok(())
}

/// A focus that left the layout moves to the first session.
fn fix_focus(g: &mut Group) {
    let ids = g.layout.sessions();
    if g.focus.as_ref().is_none_or(|f| !ids.contains(f)) {
        g.focus = ids.into_iter().find(|s| !is_slot(s));
    }
}

pub struct NewGroup {
    pub name: Option<String>,
    pub sessions: Vec<String>,
    pub dir: Option<SplitDir>,
    pub cwd: Option<PathBuf>,
}

pub async fn new(c: &Client, o: NewGroup, json: bool) -> Result<()> {
    let all = c.list_sessions().await?;
    let ids = ids(&all, &o.sessions)?;
    let name = match o.name {
        Some(n) => n,
        None if !ids.is_empty() => auto_name(&ids, &all),
        None => bail!("an empty group needs a name: --name"),
    };
    if name.trim().is_empty() {
        bail!("a group needs a name");
    }
    if ids.len() > MAX_PANES {
        bail!("a group holds {MAX_PANES} panes at most");
    }
    let layout = match (ids.is_empty(), o.dir) {
        (true, _) => layout::slot(),
        (false, None) => layout::build(&ids),
        (false, Some(dir)) => layout::chain(&ids, dir),
    };
    // An empty group needs a folder, or the app drops it as stale.
    let cwd = if o.cwd.is_some() || ids.is_empty() { Some(folder(o.cwd)?) } else { None };
    let groups = c.list_groups().await?;
    let g = Group {
        id: String::new(),
        name,
        focus: ids.first().cloned(),
        layout,
        cwd,
    };
    let saved = c.save_group(g).await?;
    release(c, &groups, &saved.id, &ids, &all).await?;
    if json {
        return print::json(&saved);
    }
    println!("{}", saved.id);
    Ok(())
}

/// Where `group add` puts a session.
pub struct Place {
    pub split: Option<Side>,
    pub of: Option<String>,
}

pub async fn add(c: &Client, group: &str, session: &str, p: Place) -> Result<()> {
    let all = c.list_sessions().await?;
    let id = resolve::session(&all, session)?.id.clone();
    let groups = c.list_groups().await?;
    let g = resolve::group(&groups, group)?.clone();
    place(c, &groups, g, &id, p, &all).await
}

/// The empty pane `p` fills in `g`, if any: with no `--of`, the first slot,
/// unless a split was asked for and the group holds a session.
fn slot_for<'a>(g: &Group, panes: &'a [String], p: &Place) -> Option<&'a String> {
    let slot = panes.iter().find(|s| is_slot(s))?;
    (p.of.is_none() && (p.split.is_none() || layout::filled(&g.layout).is_empty())).then_some(slot)
}

/// Fails when `p` needs a new pane and `g` has no room for one.
pub fn room(g: &Group, p: &Place) -> Result<()> {
    let panes = g.layout.sessions();
    if slot_for(g, &panes, p).is_none() && panes.len() >= MAX_PANES {
        bail!("group {} is full: {MAX_PANES} panes at most", g.name);
    }
    Ok(())
}

/// Puts session `id` into `g`. With no side and no target, it fills the
/// group's first slot, else the group takes the next template shape. A side
/// or a target splits that pane in half: the focus, else the first session.
pub async fn place(c: &Client, groups: &[Group], g: Group, id: &str, p: Place, all: &[SessionInfo]) -> Result<()> {
    let panes = g.layout.sessions();
    if panes.iter().any(|s| s == id) {
        bail!("session {} is already in group {}", print::short_id(id), g.name);
    }
    let mut next = g.clone();
    room(&g, &p)?;
    match slot_for(&g, &panes, &p) {
        Some(slot) => {
            layout::fill(&mut next.layout, slot, id);
        }
        // No side and no target: the next template, as the app's `addToGroup`.
        // 2 panes: the new one goes under the second. Else the default shape.
        None if p.split.is_none() && p.of.is_none() => {
            if panes.len() == 2 {
                layout::split_pane(&mut next.layout, &panes[1], Side::Down, id);
            } else {
                let mut ids = panes.clone();
                ids.push(id.to_string());
                next.layout = layout::build(&ids);
            }
        }
        None => {
            let target = match &p.of {
                Some(of) => {
                    let t = resolve::session(all, of)?.id.clone();
                    if !panes.contains(&t) {
                        bail!("session {} is not in group {}", print::short_id(&t), g.name);
                    }
                    t
                }
                None => {
                    // As the app: the focus if it holds a session, else the
                    // first session.
                    let filled = layout::filled(&g.layout);
                    g.focus
                        .clone()
                        .filter(|f| filled.contains(f))
                        .or_else(|| filled.first().cloned())
                        .or_else(|| panes.last().cloned())
                        .context("group has no panes")?
                }
            };
            layout::split_pane(&mut next.layout, &target, p.split.unwrap_or(Side::Right), id);
        }
    }
    next.focus = Some(id.to_string());
    follow_name(&mut next, &g.layout, all);
    c.save_group(next).await?;
    release(c, groups, &g.id, &[id.to_string()], all).await
}

pub async fn rm(c: &Client, group: &str, session: &str) -> Result<()> {
    let all = c.list_sessions().await?;
    let groups = c.list_groups().await?;
    let g = resolve::group(&groups, group)?;
    // A session that is gone can still be named by its id in the layout.
    let id = match resolve::session(&all, session) {
        Ok(s) => s.id.clone(),
        Err(e) => g.layout.sessions().into_iter().find(|s| s.starts_with(session)).ok_or(e)?,
    };
    if !g.layout.sessions().contains(&id) {
        bail!("session {} is not in group {}", print::short_id(&id), g.name);
    }
    let mut next = g.clone();
    next.layout = layout::remove(&g.layout, &id);
    if next.cwd.is_none() && layout::filled(&next.layout).is_empty() {
        next.cwd = all.iter().find(|s| s.id == id).map(|s| s.cwd.to_string_lossy().into_owned());
    }
    fix_focus(&mut next);
    follow_name(&mut next, &g.layout, &all);
    c.save_group(next).await?;
    Ok(())
}

pub async fn rename(c: &Client, group: &str, name: &str) -> Result<()> {
    if name.trim().is_empty() {
        bail!("a group needs a name");
    }
    let groups = c.list_groups().await?;
    let mut g = resolve::group(&groups, group)?.clone();
    g.name = name.to_string();
    c.save_group(g).await?;
    Ok(())
}

/// Resolves every argument first, so a typo deletes nothing. The sessions
/// keep running.
pub async fn delete(c: &Client, args: &[String]) -> Result<()> {
    let groups = c.list_groups().await?;
    let mut ids: Vec<String> = Vec::new();
    for a in args {
        let id = resolve::group(&groups, a)?.id.clone();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    for id in &ids {
        c.delete_group(id).await?;
    }
    Ok(())
}
