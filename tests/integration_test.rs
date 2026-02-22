#[test]
fn test_config_validation() {
    use leshy::config::Config;
    use std::path::PathBuf;

    // Valid config should load
    let valid_path = PathBuf::from("tests/fixtures/test_config.toml");
    let config = Config::from_file(&valid_path);
    assert!(config.is_ok());

    // Config with empty default_upstream should fail
    let invalid_config = r#"
[server]
listen_address = "127.0.0.1:53"
default_upstream = []
    "#;

    let temp_dir = tempfile::tempdir().unwrap();
    let invalid_path = temp_dir.path().join("invalid.toml");
    std::fs::write(&invalid_path, invalid_config).unwrap();

    let result = Config::from_file(&invalid_path);
    assert!(result.is_err());
}
