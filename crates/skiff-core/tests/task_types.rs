use skiff_core::config;

// Its own test binary: SKIFF_CONFIG is process-wide.
#[test]
fn task_types_round_trip_in_file_order() {
    let dir = std::env::temp_dir().join(format!("skiff-task-types-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("projects.toml");
    std::env::set_var("SKIFF_CONFIG", &file);
    // A hand-written file: its comments and the project stay.
    std::fs::write(&file, "# mine\n[skiff_agent]\nenabled = true\n\n[[project]]\nname = \"app\"\npath = \"/tmp/app\"\n").unwrap();
    config::set_agent_command("aider", "aider --model sonnet").unwrap();

    config::add_task_type("spec", "claude", Some("opus")).unwrap();
    config::add_task_type("review", "codex", Some(" gpt-5-codex ")).unwrap();
    config::add_task_type("test", "claude", Some("")).unwrap();
    // A custom agent saves no model: the command has it.
    config::add_task_type("docs", "aider", Some("two words")).unwrap();
    let rows = |file| config::load_from(file).unwrap().skiff_agent.tasks.0;
    let names = |file| rows(file).into_iter().map(|(k, r)| (k, r.agent, r.model)).collect::<Vec<_>>();
    let s = |v: &str| v.to_string();
    assert_eq!(
        names(&file),
        [
            (s("spec"), s("claude"), Some(s("opus"))),
            (s("review"), s("codex"), Some(s("gpt-5-codex"))),
            (s("test"), s("claude"), None),
            (s("docs"), s("aider"), None),
        ]
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("# mine\n[skiff_agent]\nenabled = true\n") && text.contains("[skiff_agent.tasks]") && text.contains("spec = { agent = \"claude\", model = \"opus\" }"), "{text}");
    assert!(text.find("[skiff_agent.tasks]").unwrap() < text.find("[[project]]").unwrap(), "{text}");

    // Refusals change nothing.
    assert!(config::add_task_type("spec", "claude", None).unwrap_err().to_string().contains("exists"));
    assert!(config::add_task_type("Bad Name", "claude", None).is_err());
    assert!(config::add_task_type("x", "nobody", None).unwrap_err().to_string().contains("no agent nobody"));
    assert!(config::add_task_type("x", "claude", Some("two words")).is_err());
    assert!(config::rename_task_type("spec", "review").unwrap_err().to_string().contains("exists"));
    assert!(config::rename_task_type("spec", "").is_err());
    assert_eq!(rows(&file).len(), 4);

    // An edit and a rename keep the row's place.
    config::set_task_type("review", "claude", None).unwrap();
    config::rename_task_type("spec", "design").unwrap();
    assert_eq!(
        names(&file).into_iter().map(|(k, a, m)| format!("{k}:{a}:{}", m.unwrap_or_default())).collect::<Vec<_>>(),
        ["design:claude:opus", "review:claude:", "test:claude:", "docs:aider:"]
    );

    // Renaming an agent moves its task types with it.
    config::rename_agent("aider", "aider2").unwrap();
    assert_eq!(config::load().unwrap().skiff_agent.tasks.get("docs").unwrap().agent, "aider2");

    // A hand edit shows on the next read, in its order.
    let text = std::fs::read_to_string(&file).unwrap().replace("[skiff_agent.tasks]\n", "[skiff_agent.tasks]\nfirst = { agent = \"codex\" }\n");
    std::fs::write(&file, text).unwrap();
    assert_eq!(rows(&file)[0].0, "first");

    for k in ["first", "design", "review", "test", "docs"] {
        config::remove_task_type(k).unwrap();
    }
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("tasks") && text.contains("enabled = true") && text.contains("[[project]]"), "{text}");
    std::fs::remove_dir_all(&dir).unwrap();
}
