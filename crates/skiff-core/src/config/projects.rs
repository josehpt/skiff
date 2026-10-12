use std::path::Path;
use anyhow::{Context, Result};
use super::types::{FolderInfo, ProjectConfig, Layout, short_name};
use super::persistence::{load, config_path, edit_lock, home_relative, expand_home, write_atomic};

/// Accent colors for new projects. Kept clear of the state colors.
const PALETTE: [&str; 8] = [
    "#b69cff", "#f28fd0", "#7ee0cb", "#e0c07e", "#9ec1ff", "#ff9e7a", "#c3e88d", "#d0c2ff",
];

/// A random item of `all` that `used` rejects, or of all of them when every
/// one is used. `all` is not empty.
fn pick_unused<T: AsRef<str>>(all: &[T], used: impl Fn(&str) -> bool) -> String {
    use std::hash::{BuildHasher, Hasher};
    let free: Vec<&T> = all.iter().filter(|c| !used(c.as_ref())).collect();
    let from: Vec<&T> = if free.is_empty() { all.iter().collect() } else { free };
    let roll = std::collections::hash_map::RandomState::new().build_hasher().finish();
    from[(roll % from.len() as u64) as usize].as_ref().to_string()
}

pub fn inspect_folder(dir: &Path) -> FolderInfo {
    let cfg = load().unwrap_or_default();
    let color = pick_unused(&PALETTE, |c| cfg.projects.iter().any(|p| p.color.as_deref() == Some(c)));
    let mut info = FolderInfo {
        root: None,
        name: String::new(),
        short: String::new(),
        color,
        branch: None,
        worktrees: 0,
        error: None,
        icon: None,
        icon_path: None,
    };
    let top = match crate::git::toplevel(&expand_home(dir)) {
        Ok(r) => r,
        Err(e) => {
            info.error = Some(format!("{e:#}"));
            return info;
        }
    };
    // Inside a linked worktree, the project is the main worktree.
    let worktrees = crate::git::list_worktrees(&top).unwrap_or_default();
    let root = worktrees
        .iter()
        .find(|w| w.is_main)
        .map(|w| w.path.clone())
        .unwrap_or(top);
    info.name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    info.short = short_name(&info.name);
    info.branch = worktrees.iter().find(|w| w.is_main).and_then(|w| w.branch.clone());
    info.worktrees = worktrees.len();
    // A closed project opens again: add_project reopens it.
    if let Some(p) = cfg
        .projects
        .iter()
        .find(|p| !p.closed && crate::project::same_path(&p.path, &root))
    {
        info.error = Some(format!("Already added as the project {}.", p.name));
    }
    if let Some(f) = crate::project::icon_file(&root, None) {
        info.icon = crate::project::data_url(&f);
        info.icon_path = f.strip_prefix(&root).ok().map(|p| p.display().to_string());
    }
    info.root = Some(root);
    info
}

/// Appends a `[[project]]` for the git repository that contains `dir`.
/// Comments and layout in the file are kept. A closed project for that
/// repository opens again with its own settings; the arguments do not apply.
pub fn add_project(
    dir: &Path,
    name: Option<String>,
    short: Option<String>,
    color: Option<String>,
    icon: Option<String>,
    agents: Vec<String>,
) -> Result<ProjectConfig> {
    let info = inspect_folder(dir);
    if let Some(e) = info.error {
        anyhow::bail!(e);
    }
    let path = info.root.expect("root is set when there is no error");
    let cfg = load()?;
    if let Some(p) = cfg.projects.iter().find(|p| crate::project::same_path(&p.path, &path)) {
        if !p.closed {
            anyhow::bail!("Already added as the project {}.", p.name);
        }
        set_project_closed(&p.name, false)?;
        return Ok(ProjectConfig { closed: false, ..p.clone() });
    }
    let _edit = edit_lock();
    let name = name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or(info.name);
    if name.is_empty() {
        anyhow::bail!("type a project name");
    }
    if cfg.projects.iter().any(|p| p.name == name) {
        anyhow::bail!("a project named {name} already exists");
    }
    let short = short
        .map(|s| s.trim().chars().take(3).collect::<String>())
        .filter(|s| !s.is_empty() && *s != short_name(&name));
    let color = color.filter(|c| !c.is_empty()).unwrap_or(info.color);
    // A theme of its own, so a new project does not look like the app or the others.
    let theme = pick_unused(&crate::theme::builtin_ids(), |t| {
        cfg.appearance.theme.as_deref() == Some(t) || cfg.projects.iter().any(|p| p.theme.as_deref() == Some(t))
    });

    let file = config_path();
    let text = if file.exists() {
        std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?
    } else {
        String::new()
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parse {}", file.display()))?;
    let mut table = toml_edit::Table::new();
    table["name"] = toml_edit::value(name.as_str());
    if let Some(short) = &short {
        table["short"] = toml_edit::value(short.as_str());
    }
    table["path"] = toml_edit::value(home_relative(&path));
    table["color"] = toml_edit::value(color.as_str());
    table["theme"] = toml_edit::value(theme.as_str());
    // No key: detect. "" : no icon. A path: that file.
    if let Some(i) = &icon {
        table["icon"] = toml_edit::value(i.as_str());
    }
    // No list means every installed agent is offered.
    if !agents.is_empty() {
        let mut list = toml_edit::Array::new();
        for a in &agents {
            list.push(a.as_str());
        }
        table["agents"] = toml_edit::value(list);
    }
    doc.entry("project")
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(Default::default()))
        .as_array_of_tables_mut()
        .ok_or_else(|| anyhow::anyhow!("`project` in {} is not [[project]] tables", file.display()))?
        .push(table);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(&file, &doc.to_string())?;

    Ok(ProjectConfig {
        name,
        short,
        path,
        color: Some(color),
        icon,
        background: None,
        background_opacity: None,
        editor: None,
        agents,
        layout: Layout::default(),
        server: None,
        files: false,
        changes: None,
        theme: Some(theme),
        closed: false,
        fleet: None,
    })
}

/// Skiff detects an icon again; `Some("")` means no icon.
pub fn set_project_icon(project: &str, icon: Option<&str>) -> Result<()> {
    set_project_key(project, "icon", icon)
}

/// Longest rail label, in characters. The rail chip has room for three.
pub const SHORT_MAX: usize = 3;

/// Sets `short`, the rail label, on the named project. An empty label removes
/// the key, so the label is the first three letters of the name again.
pub fn set_project_short(project: &str, short: &str) -> Result<()> {
    let short = short.trim();
    if short.chars().count() > SHORT_MAX {
        anyhow::bail!("the initials take at most {SHORT_MAX} characters");
    }
    set_project_key(project, "short", (!short.is_empty()).then_some(short))
}

/// Sets `fleet` on the named project. An empty name removes the key, and the
/// project stands on the rail by itself again.
pub fn set_project_fleet(project: &str, fleet: &str) -> Result<()> {
    let fleet = fleet.trim();
    set_project_key(project, "fleet", (!fleet.is_empty()).then_some(fleet))
}

/// Sets or removes `background` on the named project.
pub fn set_project_background(project: &str, background: Option<&str>) -> Result<()> {
    set_project_key(project, "background", background)
}

/// Sets `background_opacity` in percent on the named project, or removes the key.
pub fn set_project_background_opacity(project: &str, percent: Option<u8>) -> Result<()> {
    set_project_key(project, "background_opacity", percent.map(|v| i64::from(v.clamp(40, 100))))
}

/// Sets `color` on the named project.
pub fn set_project_color(project: &str, color: &str) -> Result<()> {
    let hex = color.strip_prefix('#').unwrap_or("");
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("not a #rrggbb color: {color}");
    }
    set_project_key(project, "color", Some(color))
}

/// Sets `changes = false` on the named project, or removes the key: shown is the default.
pub fn set_project_changes(project: &str, on: bool) -> Result<()> {
    set_project_key(project, "changes", (!on).then_some(false))
}

/// Sets `theme` on the named project, or removes the key: the app theme applies.
pub fn set_project_theme(project: &str, theme: Option<&str>) -> Result<()> {
    set_project_key(project, "theme", theme)
}

/// Sets `files = true` on the named project, or removes the key.
pub fn set_project_files(project: &str, on: bool) -> Result<()> {
    set_project_key(project, "files", on.then_some(true))
}

/// Sets `closed = true` on the named project, or removes the key to open it again.
pub fn set_project_closed(project: &str, closed: bool) -> Result<()> {
    set_project_key(project, "closed", closed.then_some(true))
}

/// Drops the named project's `[[project]]` table and its settings. The
/// folder stays. Comments and layout around the other tables are kept.
pub fn remove_project(project: &str) -> Result<()> {
    let _edit = edit_lock();
    let file = config_path();
    let text = std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parse {}", file.display()))?;
    let tables = doc
        .get_mut("project")
        .and_then(|i| i.as_array_of_tables_mut())
        .ok_or_else(|| anyhow::anyhow!("no [[project]] in {}", file.display()))?;
    let i = tables
        .iter()
        .position(|t| t.get("name").and_then(|n| n.as_str()) == Some(project))
        .ok_or_else(|| anyhow::anyhow!("no such project: {project}"))?;
    tables.remove(i);
    if tables.is_empty() {
        doc.remove("project");
    }
    write_atomic(&file, &doc.to_string())
}

/// Rewrites the `[[project]]` order. Comments on each table move with it.
pub fn reorder_projects(order: &[String]) -> Result<()> {
    arrange_projects(order, &[])
}

/// Sets `fleet` on each named project and rewrites the `[[project]]` order,
/// in one write. An empty fleet name removes the key. A drag on the rail can
/// move a project and change its fleet at once.
pub fn arrange_projects(order: &[String], fleets: &[(String, String)]) -> Result<()> {
    let _edit = edit_lock();
    let file = config_path();
    let text = std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parse {}", file.display()))?;
    let tables = doc
        .get_mut("project")
        .and_then(|i| i.as_array_of_tables_mut())
        .ok_or_else(|| anyhow::anyhow!("no [[project]] in {}", file.display()))?;
    let name = |t: &toml_edit::Table| t.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
    for t in tables.iter_mut() {
        let Some((_, fleet)) = fleets.iter().find(|(p, _)| *p == name(t)) else { continue };
        match fleet.trim() {
            "" => {
                t.remove("fleet");
            }
            f => {
                t.insert("fleet", toml_edit::value(f));
            }
        }
    }
    let mut old: Vec<toml_edit::Table> = tables.iter().cloned().collect();
    let positions: Vec<Option<_>> = old.iter().map(|t| t.position()).collect();
    let mut sorted = Vec::with_capacity(old.len());
    for n in order {
        if let Some(i) = old.iter().position(|t| name(t) == *n) {
            sorted.push(old.remove(i));
        }
    }
    sorted.append(&mut old);
    tables.clear();
    for (mut t, pos) in sorted.into_iter().zip(positions) {
        t.set_position(pos);
        tables.push(t);
    }
    write_atomic(&file, &doc.to_string())
}

fn set_project_key(project: &str, key: &str, value: Option<impl Into<toml_edit::Value>>) -> Result<()> {
    let _edit = edit_lock();
    let file = config_path();
    let text = std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parse {}", file.display()))?;
    let tables = doc
        .get_mut("project")
        .and_then(|i| i.as_array_of_tables_mut())
        .ok_or_else(|| anyhow::anyhow!("no [[project]] in {}", file.display()))?;
    let table = tables
        .iter_mut()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(project))
        .ok_or_else(|| anyhow::anyhow!("no such project: {project}"))?;
    match value {
        Some(v) => table[key] = toml_edit::value(v),
        None => {
            table.remove(key);
        }
    }
    write_atomic(&file, &doc.to_string())
}
