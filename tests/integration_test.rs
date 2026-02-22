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

#[test]
fn test_exclusive_zone_config_validation() {
    use leshy::config::{Config, ZoneMode};

    let config_str = r#"
[server]
listen_address = "127.0.0.1:15360"
default_upstream = ["8.8.8.8:53"]

[[zones]]
name = "corporate"
dns_servers = []
route_type = "via"
route_target = "192.168.1.1"
domains = ["internal.company.com"]
patterns = []

[[zones]]
name = "vpn-all"
mode = "exclusive"
dns_servers = []
route_type = "via"
route_target = "192.168.1.1"
domains = ["google.com"]
patterns = ['\.ru$']
    "#;

    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("exclusive.toml");
    std::fs::write(&path, config_str).unwrap();

    let config = Config::from_file(&path).unwrap();
    assert_eq!(config.zones.len(), 2);
    assert_eq!(config.zones[0].mode, ZoneMode::Inclusive);
    assert_eq!(config.zones[1].mode, ZoneMode::Exclusive);
}

#[test]
fn test_exclusive_zone_empty_exclusions_valid() {
    use leshy::config::Config;

    let config_str = r#"
[server]
listen_address = "127.0.0.1:15361"
default_upstream = ["8.8.8.8:53"]

[[zones]]
name = "catch-all"
mode = "exclusive"
dns_servers = []
route_type = "via"
route_target = "192.168.1.1"
domains = []
patterns = []
    "#;

    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("exclusive-empty.toml");
    std::fs::write(&path, config_str).unwrap();

    let result = Config::from_file(&path);
    assert!(
        result.is_ok(),
        "Exclusive zone with empty exclusions should be valid"
    );
}

#[test]
fn test_inclusive_zone_still_requires_matchers() {
    use leshy::config::Config;

    let config_str = r#"
[server]
listen_address = "127.0.0.1:15362"
default_upstream = ["8.8.8.8:53"]

[[zones]]
name = "empty-inclusive"
dns_servers = []
route_type = "via"
route_target = "192.168.1.1"
domains = []
patterns = []
    "#;

    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("inclusive-empty.toml");
    std::fs::write(&path, config_str).unwrap();

    let result = Config::from_file(&path);
    assert!(
        result.is_err(),
        "Inclusive zone with no matchers should fail validation"
    );
}

#[test]
fn test_invalid_regex_in_config_fails() {
    use leshy::config::Config;

    let config_str = r#"
[server]
listen_address = "127.0.0.1:15363"
default_upstream = ["8.8.8.8:53"]

[[zones]]
name = "bad-regex"
mode = "exclusive"
dns_servers = []
route_type = "via"
route_target = "192.168.1.1"
domains = []
patterns = ["[bad"]
    "#;

    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("bad-regex.toml");
    std::fs::write(&path, config_str).unwrap();

    let result = Config::from_file(&path);
    assert!(
        result.is_err(),
        "Invalid regex pattern should fail validation"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("bad-regex"),
        "Error should mention zone name: {err}"
    );
}
