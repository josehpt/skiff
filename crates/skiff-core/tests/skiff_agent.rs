use skiff_core::config::{self, SkiffAgentKey};

// Its own test binary: SKIFF_CONFIG is process-wide.
#[test]
fn skiff_agent_keys_round_trip() {
    let dir = std::env::temp_dir().join(format!("skiff-agent-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("SKIFF_CONFIG", dir.join("projects.toml"));

    assert_eq!(config::load().unwrap().skiff_agent, config::SkiffAgent::default());

    config::set_skiff_agent(SkiffAgentKey::Name(Some(" Bosun "))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::Agent(Some("codex"))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::Folder(Some(&dir))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::ContinueOnRestore(true)).unwrap();
    config::set_skiff_agent(SkiffAgentKey::Enabled(true)).unwrap();
    let a = config::load().unwrap().skiff_agent;
    assert_eq!(a.name.as_deref(), Some("Bosun"));
    assert_eq!(a.agent.as_deref(), Some("codex"));
    assert_eq!(a.folder.as_deref(), Some(dir.as_path()));
    assert!(a.continue_on_restore);
    assert!(a.enabled);

    // Empty or off takes the key out, so the default applies.
    config::set_skiff_agent(SkiffAgentKey::Name(Some(""))).unwrap();
    config::set_skiff_agent(SkiffAgentKey::ContinueOnRestore(false)).unwrap();
    config::set_skiff_agent(SkiffAgentKey::Enabled(false)).unwrap();
    let text = std::fs::read_to_string(dir.join("projects.toml")).unwrap();
    assert!(!text.contains("name") && !text.contains("continue_on_restore") && !text.contains("enabled"), "{text}");

    std::fs::remove_dir_all(&dir).unwrap();
}
