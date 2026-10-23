use llmproxy::config::Config;

#[test]
fn example_is_valid_and_invalid_runtime_configuration_is_rejected() {
    let mut config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
    config.validate().unwrap();
    config.claude_executable = "relative/claude".into();
    assert!(config.validate().is_err());
    config.claude_executable = "/usr/local/bin/claude".into();
    config.max_concurrent_requests = 0;
    assert!(config.validate().is_err());
    assert!(toml::from_str::<Config>("unknown_setting = true").is_err());
    assert!(toml::from_str::<Config>("[[providers]]\nkind='codex'\nid='obsolete'").is_err());
}
