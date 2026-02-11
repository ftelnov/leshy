// Composable Configuration Test
// Tests loading zones from multiple config files in config.d directory

use leshy::config::Config;

#[test]
fn test_load_from_config_d() -> anyhow::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let config_path = temp_dir.path().join("main.toml");
    let config_d = temp_dir.path().join("config.d");
    std::fs::create_dir(&config_d)?;

    // Main config with server settings
    let main_config = r#"
[server]
listen_address = "127.0.0.1:15390"
default_upstream = ["8.8.8.8:53"]
route_failure_mode = "fallback"
auto_reload = false

# Main config can have zones too
[[zones]]
name = "main_zone"
dns_servers = []
route_type = "via"
route_target = "192.168.1.1"
domains = ["main.local"]
patterns = []
    "#;

    std::fs::write(&config_path, main_config)?;

    // Zone file 1
    let zone1 = r#"
[[zones]]
name = "zone1"
dns_servers = []
route_type = "via"
route_target = "192.168.2.1"
domains = ["zone1.local"]
patterns = []
    "#;

    std::fs::write(config_d.join("10-zone1.toml"), zone1)?;

    // Zone file 2
    let zone2 = r#"
[[zones]]
name = "zone2"
dns_servers = []
route_type = "dev"
route_target = "/tmp/zone2.dev"
domains = ["zone2.local"]
patterns = []

[[zones]]
name = "zone3"
dns_servers = []
route_type = "via"
route_target = "192.168.3.1"
domains = ["zone3.local"]
patterns = []
    "#;

    std::fs::write(config_d.join("20-zone2-and-3.toml"), zone2)?;

    // Load config with includes
    let config = Config::from_file_with_includes(&config_path)?;

    // Should have 4 zones: main_zone, zone1, zone2, zone3
    assert_eq!(config.zones.len(), 4, "Should have 4 zones total");

    let zone_names: Vec<String> = config.zones.iter().map(|z| z.name.clone()).collect();
    assert!(zone_names.contains(&"main_zone".to_string()));
    assert!(zone_names.contains(&"zone1".to_string()));
    assert!(zone_names.contains(&"zone2".to_string()));
    assert!(zone_names.contains(&"zone3".to_string()));

    println!(
        "✓ Composable config test passed! Loaded {} zones",
        config.zones.len()
    );

    Ok(())
}

#[test]
fn test_config_d_sorting() -> anyhow::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let config_path = temp_dir.path().join("main.toml");
    let config_d = temp_dir.path().join("config.d");
    std::fs::create_dir(&config_d)?;

    // Main config
    let main_config = r#"
[server]
listen_address = "127.0.0.1:15391"
default_upstream = ["8.8.8.8:53"]
    "#;

    std::fs::write(&config_path, main_config)?;

    // Create files in non-alphabetical order
    std::fs::write(
        config_d.join("50-last.toml"),
        r#"
[[zones]]
name = "last"
route_type = "via"
route_target = "192.168.1.1"
domains = ["last.local"]
patterns = []
    "#,
    )?;

    std::fs::write(
        config_d.join("10-first.toml"),
        r#"
[[zones]]
name = "first"
route_type = "via"
route_target = "192.168.1.1"
domains = ["first.local"]
patterns = []
    "#,
    )?;

    std::fs::write(
        config_d.join("30-middle.toml"),
        r#"
[[zones]]
name = "middle"
route_type = "via"
route_target = "192.168.1.1"
domains = ["middle.local"]
patterns = []
    "#,
    )?;

    let config = Config::from_file_with_includes(&config_path)?;

    // Should be sorted: first, middle, last
    assert_eq!(config.zones[0].name, "first");
    assert_eq!(config.zones[1].name, "middle");
    assert_eq!(config.zones[2].name, "last");

    println!("✓ Config.d sorting test passed!");

    Ok(())
}

#[test]
fn test_config_without_config_d() -> anyhow::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let config_path = temp_dir.path().join("main.toml");

    // Main config without config.d directory
    let main_config = r#"
[server]
listen_address = "127.0.0.1:15392"
default_upstream = ["8.8.8.8:53"]

[[zones]]
name = "only_zone"
route_type = "via"
route_target = "192.168.1.1"
domains = ["only.local"]
patterns = []
    "#;

    std::fs::write(&config_path, main_config)?;

    // Load config (no config.d exists)
    let config = Config::from_file_with_includes(&config_path)?;

    // Should work fine with just main config
    assert_eq!(config.zones.len(), 1);
    assert_eq!(config.zones[0].name, "only_zone");

    println!("✓ Config without config.d test passed!");

    Ok(())
}

#[test]
fn test_invalid_zone_file_skipped() -> anyhow::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let config_path = temp_dir.path().join("main.toml");
    let config_d = temp_dir.path().join("config.d");
    std::fs::create_dir(&config_d)?;

    // Main config
    let main_config = r#"
[server]
listen_address = "127.0.0.1:15393"
default_upstream = ["8.8.8.8:53"]
    "#;

    std::fs::write(&config_path, main_config)?;

    // Valid zone file
    std::fs::write(
        config_d.join("10-valid.toml"),
        r#"
[[zones]]
name = "valid"
route_type = "via"
route_target = "192.168.1.1"
domains = ["valid.local"]
patterns = []
    "#,
    )?;

    // Invalid zone file (malformed TOML)
    std::fs::write(
        config_d.join("20-invalid.toml"),
        "this is not valid toml {[",
    )?;

    // Another valid zone file
    std::fs::write(
        config_d.join("30-valid2.toml"),
        r#"
[[zones]]
name = "valid2"
route_type = "via"
route_target = "192.168.1.1"
domains = ["valid2.local"]
patterns = []
    "#,
    )?;

    // Should load valid zones and skip invalid one
    let config = Config::from_file_with_includes(&config_path)?;

    assert_eq!(config.zones.len(), 2);
    assert_eq!(config.zones[0].name, "valid");
    assert_eq!(config.zones[1].name, "valid2");

    println!("✓ Invalid zone file skipping test passed!");

    Ok(())
}

#[test]
fn test_duplicate_zone_names_detected() -> anyhow::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let config_path = temp_dir.path().join("main.toml");
    let config_d = temp_dir.path().join("config.d");
    std::fs::create_dir(&config_d)?;

    // Main config
    let main_config = r#"
[server]
listen_address = "127.0.0.1:15394"
default_upstream = ["8.8.8.8:53"]

[[zones]]
name = "duplicate"
route_type = "via"
route_target = "192.168.1.1"
domains = ["dup1.local"]
patterns = []
    "#;

    std::fs::write(&config_path, main_config)?;

    // Zone file with duplicate name
    std::fs::write(
        config_d.join("10-dup.toml"),
        r#"
[[zones]]
name = "duplicate"
route_type = "via"
route_target = "192.168.2.1"
domains = ["dup2.local"]
patterns = []
    "#,
    )?;

    // Should fail validation due to duplicate
    let result = Config::from_file_with_includes(&config_path);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Duplicate zone name"));

    println!("✓ Duplicate zone name detection test passed!");

    Ok(())
}

#[test]
fn test_non_toml_files_ignored() -> anyhow::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let config_path = temp_dir.path().join("main.toml");
    let config_d = temp_dir.path().join("config.d");
    std::fs::create_dir(&config_d)?;

    // Main config
    let main_config = r#"
[server]
listen_address = "127.0.0.1:15395"
default_upstream = ["8.8.8.8:53"]
    "#;

    std::fs::write(&config_path, main_config)?;

    // Zone file
    std::fs::write(
        config_d.join("zone.toml"),
        r#"
[[zones]]
name = "valid"
route_type = "via"
route_target = "192.168.1.1"
domains = ["valid.local"]
patterns = []
    "#,
    )?;

    // Non-.toml files (should be ignored)
    std::fs::write(config_d.join("README.md"), "# Documentation")?;
    std::fs::write(config_d.join("backup.txt"), "old config")?;
    std::fs::write(config_d.join(".hidden"), "hidden file")?;

    let config = Config::from_file_with_includes(&config_path)?;

    // Should only load the .toml file
    assert_eq!(config.zones.len(), 1);
    assert_eq!(config.zones[0].name, "valid");

    println!("✓ Non-TOML files ignored test passed!");

    Ok(())
}
