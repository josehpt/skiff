//! Terminal color themes: built in, or dropped as files into
//! `<config dir>/skiff/themes/`. A project picks one by id.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum ColorFamily {
    Neutral, Beige, Red, Orange, Yellow, Green, Cyan, Blue, Purple, Pink,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TerminalTheme {
    /// `builtin:<slug>` or `file:<file name>`. Old `foot:<slug>` ids mean `builtin:<slug>`.
    pub id: String,
    pub name: String,
    /// "built-in" or "file".
    pub source: String,
    pub foreground: String,
    pub background: String,
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_foreground: Option<String>,
    pub selection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_foreground: Option<String>,
    /// Explicit visual category; never inferred from a nearly black background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_family: Option<ColorFamily>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main_color: Option<String>,
    /// ANSI 0–15: regular 0–7, then bright 0–7. `#rrggbb`.
    pub palette: Vec<String>,
    /// App colors a theme file brings (Superset's `ui` block: background,
    /// card, sidebar, border, mutedForeground, primary, warning, destructive,
    /// ...). Without it the app derives its colors from the palette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<std::collections::BTreeMap<String, String>>,
}

/// The default when a project names no theme.
pub const DEFAULT_THEME: &str = "builtin:foot";

const BUILTIN: &[(&str, &str)] = &[
    ("foot", include_str!("../themes/foot.toml")),
    ("default", include_str!("../themes/default.toml")),
    ("neon-tropic", include_str!("../themes/neon-tropic.toml")),
    ("moonlight", include_str!("../themes/moonlight.toml")),
    ("ember", include_str!("../themes/ember.toml")),
    ("lagoon", include_str!("../themes/lagoon.toml")),
    ("violet-pulse", include_str!("../themes/violet-pulse.toml")),
    ("manila-brief", include_str!("../themes/manila-brief.toml")),
    ("navy-cut", include_str!("../themes/navy-cut.toml")),
    ("pad-39", include_str!("../themes/pad-39.toml")),
    ("galley-7", include_str!("../themes/galley-7.toml")),
    ("dradis", include_str!("../themes/dradis.toml")),
    ("candy", include_str!("../themes/candy.toml")),
    ("hail-mary", include_str!("../themes/hail-mary.toml")),
    ("pirate", include_str!("../themes/pirate.toml")),
    ("jungle-canopy", include_str!("../themes/jungle-canopy.toml")),
    ("macaw", include_str!("../themes/macaw.toml")),
    ("sonar", include_str!("../themes/sonar.toml")),
    ("ice-station", include_str!("../themes/ice-station.toml")),
    ("prussian", include_str!("../themes/prussian.toml")),
    ("adriatic", include_str!("../themes/adriatic.toml")),
    ("blue-note", include_str!("../themes/blue-note.toml")),
    ("flare", include_str!("../themes/flare.toml")),
    ("brick", include_str!("../themes/brick.toml")),
    ("afterburner", include_str!("../themes/afterburner.toml")),
    ("lemon-drop", include_str!("../themes/lemon-drop.toml")),
    ("emerald", include_str!("../themes/emerald.toml")),
    ("riptide", include_str!("../themes/riptide.toml")),
    ("glacier", include_str!("../themes/glacier.toml")),
    ("nebula", include_str!("../themes/nebula.toml")),
    ("flamingo", include_str!("../themes/flamingo.toml")),
    ("dusty-rose", include_str!("../themes/dusty-rose.toml")),
    ("battleship", include_str!("../themes/battleship.toml")),
    ("driftwood", include_str!("../themes/driftwood.toml")),
];

/// The ids of the bundled themes, such as `builtin:foot`.
pub fn builtin_ids() -> Vec<String> {
    BUILTIN.iter().map(|(slug, _)| format!("builtin:{slug}")).collect()
}

/// The operator Foot themes, bundled as Skiff TOML, then imported theme files.
pub fn list() -> Vec<TerminalTheme> {
    let mut out: Vec<_> = BUILTIN.iter().map(|(slug, text)| {
        let mut t = parse_file(Path::new(&format!("{slug}.toml")), text)
            .expect("bundled theme is valid Skiff TOML");
        t.id = format!("builtin:{slug}");
        t.source = "built-in".into();
        t
    }).collect();
    let mut files: Vec<_> = theme_dir()
        .and_then(|d| std::fs::read_dir(d).ok())
        .map(|rd| rd.flatten().filter_map(|e| from_file(&e.path())).collect())
        .unwrap_or_default();
    files.sort_by_key(|a| a.0.name.to_lowercase());
    for (mut f, author) in files {
        if out.iter().any(|t| t.name.eq_ignore_ascii_case(&f.name)) {
            f.name = format!("{} ({})", f.name, author.unwrap_or_else(|| "file".into()));
        }
        out.push(f);
    }
    out
}

pub fn theme_dir() -> Option<PathBuf> {
    crate::config::config_path().parent().map(|d| d.join("themes"))
}

/// Copy supported themes into the theme folder. Existing files stay intact.
/// Return errors per file so one bad file does not stop a bulk import.
pub fn import_files(paths: &[PathBuf]) -> Vec<String> {
    let Some(dir) = theme_dir() else { return vec!["Cannot find the theme folder".into()] };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return vec![e.to_string()];
    }
    paths.iter().filter_map(|path| {
        import_file(path, &dir).err().map(|e| format!("{}: {e}", path.display()))
    }).collect()
}

fn import_file(path: &Path, dir: &Path) -> Result<(), String> {
    use std::io::Write;
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let valid = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
        from_json(path, &text).is_some()
    } else {
        parse_file(path, &text).is_some()
    };
    if !valid {
        return Err("Unsupported or incomplete theme. It needs foreground, background, and 16 ANSI colors".into());
    }
    let name = path.file_name().ok_or("Missing file name")?;
    let target = dir.join(name);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&target)
        .map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists {
            "A theme file with this name already exists".into()
        } else { e.to_string() })?;
    if let Err(e) = file.write_all(text.as_bytes()) {
        let _ = std::fs::remove_file(target);
        return Err(e.to_string());
    }
    Ok(())
}

/// `#rrggbb` from `rrggbb`, `#rrggbb`, or `0xrrggbb`.
fn hex(v: &str) -> Option<String> {
    let v = v.trim().trim_matches(|c| c == '"' || c == '\'');
    let v = v.strip_prefix('#').or_else(|| v.strip_prefix("0x")).unwrap_or(v);
    let v = v.get(..6)?;
    v.chars().all(|c| c.is_ascii_hexdigit()).then(|| format!("#{}", v.to_ascii_lowercase()))
}

/// Flat `key = value` or `key: value` lines, with the section they sit in.
fn pairs(text: &str) -> Vec<(String, String, String)> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split(" #").next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            section = line.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        let Some(i) = line.find(['=', ':']) else { continue };
        let (k, v) = line.split_at(i);
        out.push((section.clone(), k.trim().to_string(), v[1..].trim().to_string()));
    }
    out
}

#[allow(clippy::too_many_arguments, reason = "one field per parsed theme key")]
fn finish(
    id: String,
    name: String,
    source: &str,
    fg: Option<String>,
    bg: Option<String>,
    pal: Vec<Option<String>>,
    cursor: Option<String>,
    selection: Option<String>,
) -> Option<TerminalTheme> {
    let palette: Option<Vec<String>> = pal.into_iter().collect();
    Some(TerminalTheme {
        id,
        name,
        source: source.into(),
        foreground: fg?,
        background: bg?,
        cursor,
        cursor_foreground: None,
        selection,
        selection_foreground: None,
        color_family: None,
        main_color: None,
        palette: palette?,
        ui: None,
    })
}

fn parse_foot(text: &str, id: String, name: String, source: &str) -> Option<TerminalTheme> {
    let all = pairs(text);
    let kv: Vec<(String, String)> = all.iter().filter(|&(s, _, _)| s == "colors" || s == "colors-dark").cloned()
        .map(|(_, k, v)| (k, v))
        .collect();
    let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).and_then(|(_, v)| hex(v));
    let mut pal = Vec::new();
    for p in ["regular", "bright"] {
        for i in 0..8 {
            pal.push(get(&format!("{p}{i}")));
        }
    }
    let cursor_colors = all.iter().find(|(s, k, _)| s == "cursor" && k == "color")
        .map(|(_, _, v)| v.as_str())
        .or_else(|| kv.iter().find(|(k, _)| k == "cursor").map(|(_, v)| v.as_str()));
    let cursor = cursor_colors.and_then(|v| v.split_whitespace().nth(1).and_then(hex));
    let mut theme = finish(id, name, source, get("foreground"), get("background"), pal, cursor, get("selection-background"))?;
    theme.cursor_foreground = cursor_colors.and_then(|v| v.split_whitespace().next().and_then(hex));
    theme.selection_foreground = get("selection-foreground");
    Some(theme)
}

/// A theme file, with its author when the file names one.
fn from_file(path: &Path) -> Option<(TerminalTheme, Option<String>)> {
    let text = std::fs::read_to_string(path).ok()?;
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
        return from_json(path, &text);
    }
    parse_file(path, &text).map(|t| (t, None))
}

/// Superset's theme JSON: `name`, `author`, `terminal` (xterm names), `ui`.
fn from_json(path: &Path, text: &str) -> Option<(TerminalTheme, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let term = v.get("terminal")?.as_object()?;
    let c = |k: &str| term.get(k).and_then(|x| x.as_str()).and_then(hex);
    let names = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
    let mut pal: Vec<Option<String>> = names.iter().map(|n| c(n)).collect();
    pal.extend(names.iter().map(|n| c(&format!("bright{}{}", n[..1].to_uppercase(), &n[1..]))));
    let ui = v.get("ui").and_then(|u| u.as_object()).map(|u| {
        u.iter()
            .filter_map(|(k, x)| x.as_str().and_then(hex).map(|h| (k.clone(), h)))
            .collect()
    });
    let file = path.file_name()?.to_string_lossy().into_owned();
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let name = v.get("name").and_then(|x| x.as_str()).unwrap_or(&stem).to_string();
    let mut t = finish(format!("file:{file}"), name, "file", c("foreground"), c("background"), pal, c("cursor"), c("selectionBackground"))?;
    t.ui = ui;
    t.cursor_foreground = c("cursorAccent");
    t.selection_foreground = c("selectionForeground");
    if let Some(family) = v.get("color_family") {
        t.color_family = Some(serde_json::from_value(family.clone()).ok()?);
    }
    if let Some(color) = v.get("main_color") {
        t.main_color = Some(hex(color.as_str()?)?);
    }
    let author = v.get("author").and_then(|x| x.as_str()).map(|a| {
        // "Pavel Pertsev (port: Baris Can Sayin)" -> "Pavel Pertsev"
        a.split(" (").next().unwrap_or(a).to_string()
    });
    Some((t, author))
}

fn parse_file(path: &Path, text: &str) -> Option<TerminalTheme> {
    let file = path.file_name()?.to_string_lossy().into_owned();
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let id = format!("file:{file}");
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let kv = pairs(text);
    let named = |k: &str| kv.iter().find(|(_, kk, _)| kk == k).map(|(_, _, v)| v.trim_matches('"').to_string());
    let name = named("name").or_else(|| named("scheme")).unwrap_or_else(|| stem.clone());

    // base16 / base24 YAML: base00..base0F (and base10..base17).
    if ext == "yaml" || ext == "yml" {
        let b = |k: &str| kv.iter().find(|(_, kk, _)| kk == k).and_then(|(_, _, v)| hex(v));
        let pick = |k: &str, alt: &str| b(k).or_else(|| b(alt));
        let pal = vec![
            b("base00"), b("base08"), b("base0B"), b("base0A"), b("base0D"), b("base0E"), b("base0C"), b("base05"),
            b("base03"), pick("base12", "base08"), pick("base14", "base0B"), pick("base13", "base0A"),
            pick("base16", "base0D"), pick("base17", "base0E"), pick("base15", "base0C"), b("base07"),
        ];
        return finish(id, name, "file", b("base05"), b("base00"), pal, None, b("base02"));
    }
    // foot .ini
    if kv.iter().any(|(s, _, _)| s == "colors" || s == "colors-dark") && ext != "toml" {
        return parse_foot(text, id, name, "file");
    }
    // Skiff TOML: [terminal] foreground/background/regular/bright arrays.
    if ext == "toml" {
        let doc: toml::Table = text.parse().ok()?;
        if let Some(t) = doc.get("terminal").and_then(|v| v.as_table()) {
            let s = |k: &str| t.get(k).and_then(|v| v.as_str()).and_then(hex);
            let arr = |k: &str| -> Vec<Option<String>> {
                t.get(k)
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().map(|x| x.as_str().and_then(hex)).collect())
                    .unwrap_or_default()
            };
            let mut pal = arr("regular");
            pal.extend(arr("bright"));
            if pal.len() != 16 {
                return None;
            }
            let name = doc.get("name").and_then(|v| v.as_str()).map(String::from).unwrap_or(name);
            let mut theme = finish(id, name, "file", s("foreground"), s("background"), pal, s("cursor"), s("selection"))?;
            theme.cursor_foreground = s("cursor_foreground");
            theme.selection_foreground = s("selection_foreground");
            if let Some(family) = doc.get("color_family") {
                theme.color_family = Some(family.clone().try_into().ok()?);
            }
            if let Some(color) = doc.get("main_color") {
                theme.main_color = Some(hex(color.as_str()?)?);
            }
            return Some(theme);
        }
        // Alacritty: [colors.primary], [colors.normal], [colors.bright].
        let colors = doc.get("colors").and_then(|v| v.as_table())?;
        let sec = |s: &str, k: &str| {
            colors.get(s).and_then(|v| v.as_table()).and_then(|t| t.get(k)).and_then(|v| v.as_str()).and_then(hex)
        };
        let names = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"];
        let mut pal: Vec<Option<String>> = names.iter().map(|n| sec("normal", n)).collect();
        pal.extend(names.iter().map(|n| sec("bright", n)));
        return finish(id, name, "file", sec("primary", "foreground"), sec("primary", "background"), pal, sec("cursor", "cursor"), sec("selection", "background"));
    }
    // Ghostty: `palette = N=#hex`, background, foreground.
    if kv.iter().any(|(_, k, _)| k == "palette") {
        let mut pal = vec![None; 16];
        for (_, k, v) in &kv {
            if k == "palette" {
                if let Some((n, c)) = v.split_once('=') {
                    if let Ok(i) = n.trim().parse::<usize>() {
                        if i < 16 {
                            pal[i] = hex(c);
                        }
                    }
                }
            }
        }
        let g = |k: &str| kv.iter().find(|(_, kk, _)| kk == k).and_then(|(_, _, v)| hex(v));
        return finish(id, name, "file", g("foreground"), g("background"), pal, g("cursor-color"), g("selection-background"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_valid_themes_without_overwriting_files() {
        let dir = std::env::temp_dir().join(format!("skiff-theme-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source");
        let target = dir.join("themes");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let path = source.join("sample.toml");
        let text = "name = 'Sample'\n[terminal]\nforeground = '#112233'\nbackground = '#ffffff'\nregular = ['#000000', '#ff0000', '#00ff00', '#ffff00', '#0000ff', '#ff00ff', '#00ffff', '#ffffff']\nbright = ['#111111', '#ff1111', '#11ff11', '#ffff11', '#1111ff', '#ff11ff', '#11ffff', '#eeeeee']\n";
        std::fs::write(&path, text).unwrap();
        import_file(&path, &target).unwrap();
        assert_eq!(std::fs::read_to_string(target.join("sample.toml")).unwrap(), text);
        assert!(import_file(&path, &target).unwrap_err().contains("already exists"));
        std::fs::write(&path, "[terminal]\nbackground = '#ffffff'\n").unwrap();
        assert!(import_file(&path, &target).unwrap_err().contains("incomplete"));
        assert_eq!(std::fs::read_to_string(target.join("sample.toml")).unwrap(), text);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reads_foot_and_base16() {
        let foot = "[colors]\nbackground=08001a\nforeground=e0ccff\nregular0=000000\nregular1=ff0000\nregular2=00ff00\nregular3=ffff00\nregular4=0000ff\nregular5=ff00ff\nregular6=00ffff\nregular7=ffffff\nbright0=111111\nbright1=ff1111\nbright2=11ff11\nbright3=ffff11\nbright4=1111ff\nbright5=ff11ff\nbright6=11ffff\nbright7=eeeeee\n";
        let t = parse_foot(foot, "x".into(), "X".into(), "foot").unwrap();
        assert!(t.ui.is_none());
        assert_eq!(t.background, "#08001a");
        assert_eq!(t.palette.len(), 16);
        assert_eq!(t.palette[9], "#ff1111");
        assert_eq!(builtin_count(), 34);
        for theme in list().into_iter().filter(|t| t.source == "built-in") {
            assert!(theme.color_family.is_some(), "{}", theme.name);
            assert_eq!(theme.main_color.as_ref(), Some(&theme.foreground));
            assert_eq!(theme.cursor.as_deref(), Some("#08001a"));
            assert_eq!(theme.cursor_foreground.as_deref(), Some("#ff00ff"));
            assert_eq!(theme.selection.as_deref(), Some("#4400aa"));
            assert_eq!(theme.selection_foreground.as_deref(), Some("#ffffff"));
            assert_eq!(theme.palette.len(), 16);
        }
    }

    fn builtin_count() -> usize {
        list().iter().filter(|t| t.source == "built-in").count()
    }
}
