use skiff_core::{config, project};

// Its own test binary: SKIFF_CONFIG is process-wide.
#[test]
fn close_reopen_and_remove_keep_the_rest_of_the_file() {
    let dir = std::env::temp_dir().join(format!("skiff-close-remove-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let init = std::process::Command::new("git").arg("-C").arg(&repo).arg("init").arg("-q").status().unwrap();
    assert!(init.success());
    let file = dir.join("projects.toml");
    std::fs::write(
        &file,
        "# my projects\n\n[theme]\nfile = \"old.css\"\n\n[[project]]\nname = \"keep\" # the one that stays\npath = \"/tmp/keep\"\ncolor = \"#f28fd0\"\neditor = \"zed\"\nlayout = \"side-by-side\"\nserver = { cmd = \"npm run dev\", port_env = \"PORT\" }\n",
    )
    .unwrap();
    std::env::set_var("SKIFF_CONFIG", &file);

    let added = config::add_project(&repo, Some("repo".into()), None, Some("#8fd0f2".into()), None, vec![]).unwrap();
    assert!(added.theme.as_deref().is_some_and(|t| t.starts_with("builtin:")));
    assert!(!added.closed);

    // Close: the table and its settings stay, marked closed.
    config::set_project_closed("repo", true).unwrap();
    let cfg = config::load().unwrap();
    let p = cfg.projects.iter().find(|p| p.name == "repo").unwrap();
    assert!(p.closed);
    assert_eq!(p.color.as_deref(), Some("#8fd0f2"));
    let listed = project::list().unwrap();
    let p = listed.iter().find(|p| p.name == "repo").unwrap();
    assert!(p.closed && p.worktrees.is_empty() && p.error.is_none());

    // Adding the same folder opens it again with its own settings.
    let info = config::inspect_folder(&repo);
    assert_eq!(info.error, None);
    let back = config::add_project(&repo, Some("other name".into()), None, Some("#000000".into()), None, vec![]).unwrap();
    assert_eq!(back.name, "repo");
    assert_eq!(back.color.as_deref(), Some("#8fd0f2"));
    assert!(!config::load().unwrap().projects.iter().any(|p| p.closed));
    assert!(config::add_project(&repo, None, None, None, None, vec![]).is_err());

    // Remove: the table goes; the other table and the comments stay.
    config::remove_project("repo").unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("\"repo\""), "{text}");
    assert!(text.contains("# my projects") && text.contains("# the one that stays"), "{text}");
    for legacy in ["[theme]", "file = \"old.css\"", "editor = \"zed\"", "layout = \"side-by-side\"", "server = { cmd = \"npm run dev\", port_env = \"PORT\" }"] {
        assert!(text.contains(legacy), "{legacy}: {text}");
    }
    let names: Vec<_> = config::load().unwrap().projects.into_iter().map(|p| p.name).collect();
    assert_eq!(names, ["keep"]);
    assert!(config::remove_project("repo").is_err());

    // The last project goes: no empty [[project]] is left behind.
    config::remove_project("keep").unwrap();
    assert!(config::load().unwrap().projects.is_empty());
    assert!(repo.is_dir());

    let _ = std::fs::remove_dir_all(&dir);
}
