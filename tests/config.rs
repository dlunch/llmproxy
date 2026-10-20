use llmproxy::config::{Config, ProviderConfig};

#[test]
fn example_is_valid_and_invalid_runtime_configuration_is_rejected() {
    let mut config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
    config.validate().unwrap();
    config.providers.push(ProviderConfig::Codex {
        id: "codex-secondary".into(),
    });
    config.validate().unwrap();
    config.max_concurrent_requests = 0;
    assert!(config.validate().is_err());
    assert!(toml::from_str::<Config>("unknown_setting = true").is_err());
}
