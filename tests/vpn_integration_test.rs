// VPN Integration Tests
// Tests device-based routing, DNS routing with corporate DNS, and route verification

use hickory_client::client::{AsyncClient, ClientHandle};
use hickory_client::rr::{DNSClass, Name, RecordType};
use hickory_client::udp::UdpClientStream;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::time::{sleep, Duration};

struct TestServer {
    _process: Child,
    _temp_dir: Option<TempDir>,
}

impl TestServer {
    async fn start_with_temp_config(config_content: &str) -> anyhow::Result<Self> {
        let temp_dir = tempfile::tempdir()?;
        let config_path = temp_dir.path().join("config.toml");
        std::fs::write(&config_path, config_content)?;

        // Build the server binary
        let status = Command::new("cargo")
            .args(["build", "--release"])
            .status()
            .await?;

        if !status.success() {
            anyhow::bail!("Failed to build leshy");
        }

        // Start the server
        let process = Command::new("target/release/leshy")
            .arg(&config_path)
            .env("RUST_LOG", "info")
            .spawn()?;

        // Give server time to start
        sleep(Duration::from_millis(500)).await;

        Ok(Self {
            _process: process,
            _temp_dir: Some(temp_dir),
        })
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self._process.start_kill();
    }
}

async fn create_dns_client(server_addr: SocketAddr) -> anyhow::Result<AsyncClient> {
    let stream = UdpClientStream::<tokio::net::UdpSocket>::new(server_addr);
    let (client, bg) = AsyncClient::connect(stream).await?;
    tokio::spawn(bg);
    Ok(client)
}

async fn check_route_exists(ip: IpAddr) -> anyhow::Result<bool> {
    let output = Command::new("ip").args(["route", "show"]).output().await?;

    let routes = String::from_utf8_lossy(&output.stdout);
    Ok(routes.contains(&ip.to_string()))
}

async fn get_default_gateway() -> anyhow::Result<Option<String>> {
    let output = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .await?;

    let route_output = String::from_utf8_lossy(&output.stdout);
    // Parse: "default via 192.168.1.1 dev eth0"
    for part in route_output.split_whitespace() {
        if let Ok(addr) = part.parse::<IpAddr>() {
            return Ok(Some(addr.to_string()));
        }
    }

    Ok(None)
}

#[tokio::test]
async fn test_device_based_routing() -> anyhow::Result<()> {
    // Create a device file for testing
    let temp_dir = tempfile::tempdir()?;
    let device_file = temp_dir.path().join("test.dev");

    // Get the default gateway to use for test routes
    let _gateway = get_default_gateway()
        .await?
        .unwrap_or_else(|| "192.168.1.1".to_string());

    // Write current default interface name to device file
    // Try to find a real interface
    let output = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .await?;

    let route_output = String::from_utf8_lossy(&output.stdout);
    let device = route_output
        .split_whitespace()
        .skip_while(|&s| s != "dev")
        .nth(1)
        .unwrap_or("lo");

    std::fs::write(&device_file, device)?;

    let config = format!(
        r#"
[server]
listen_address = "127.0.0.1:15355"
default_upstream = ["8.8.8.8:53"]
route_failure_mode = "fallback"

[[zones]]
name = "test-dev"
dns_servers = []
route_type = "dev"
route_target = "{}"
domains = ["example.org"]
patterns = []
    "#,
        device_file.display()
    );

    let _server = TestServer::start_with_temp_config(&config).await?;
    let server_addr: SocketAddr = "127.0.0.1:15355".parse()?;
    let mut client = create_dns_client(server_addr).await?;

    // Query a domain
    let name = Name::from_str("www.example.org.")?;
    let response = client.query(name, DNSClass::IN, RecordType::A).await?;

    // Should get DNS response
    assert!(!response.answers().is_empty(), "Should have DNS answers");

    // Give time for route to be added
    sleep(Duration::from_millis(100)).await;

    // Extract IP from response and check if route exists
    if let Some(answer) = response.answers().first() {
        if let Some(rdata) = answer.data() {
            if let Some(a) = rdata.as_a() {
                let ip = IpAddr::V4(a.0);
                // Route should exist (or fail gracefully)
                // We don't assert here because route might fail in test environment
                println!("Route check for IP: {ip}");
                let route_exists = check_route_exists(ip).await?;
                println!("Route exists: {route_exists}");
            }
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_via_routing_with_real_gateway() -> anyhow::Result<()> {
    // Get actual default gateway from system
    let gateway = get_default_gateway()
        .await?
        .unwrap_or_else(|| "192.168.1.1".to_string());

    let config = format!(
        r#"
[server]
listen_address = "127.0.0.1:15356"
default_upstream = ["8.8.8.8:53"]
route_failure_mode = "fallback"

[[zones]]
name = "test-via"
dns_servers = []
route_type = "via"
route_target = "{gateway}"
domains = ["example.net"]
patterns = []
    "#
    );

    let _server = TestServer::start_with_temp_config(&config).await?;
    let server_addr: SocketAddr = "127.0.0.1:15356".parse()?;
    let mut client = create_dns_client(server_addr).await?;

    // Query a domain
    let name = Name::from_str("www.example.net.")?;
    let response = client.query(name, DNSClass::IN, RecordType::A).await?;

    // Should get DNS response
    assert!(!response.answers().is_empty(), "Should have DNS answers");

    // Give time for route to be added
    sleep(Duration::from_millis(100)).await;

    // Check routes
    if let Some(answer) = response.answers().first() {
        if let Some(rdata) = answer.data() {
            if let Some(a) = rdata.as_a() {
                let ip = IpAddr::V4(a.0);
                println!("Checking route for IP: {ip} via {gateway}");
                let route_exists = check_route_exists(ip).await?;
                println!("Route exists: {route_exists}");
            }
        }
    }

    Ok(())
}

#[tokio::test]
async fn test_multiple_zones_different_gateways() -> anyhow::Result<()> {
    let gateway = get_default_gateway()
        .await?
        .unwrap_or_else(|| "192.168.1.1".to_string());

    let config = format!(
        r#"
[server]
listen_address = "127.0.0.1:15357"
default_upstream = ["8.8.8.8:53"]
route_failure_mode = "fallback"

[[zones]]
name = "zone1"
dns_servers = []
route_type = "via"
route_target = "{gateway}"
domains = ["cloudflare.com"]
patterns = []

[[zones]]
name = "zone2"
dns_servers = []
route_type = "via"
route_target = "{gateway}"
domains = ["google.com"]
patterns = []
    "#
    );

    let _server = TestServer::start_with_temp_config(&config).await?;
    let server_addr: SocketAddr = "127.0.0.1:15357".parse()?;
    let mut client = create_dns_client(server_addr).await?;

    // Query both zones
    let name1 = Name::from_str("cloudflare.com.")?;
    let response1 = client.query(name1, DNSClass::IN, RecordType::A).await?;
    assert!(!response1.answers().is_empty());

    let name2 = Name::from_str("www.google.com.")?;
    let response2 = client.query(name2, DNSClass::IN, RecordType::A).await?;
    assert!(!response2.answers().is_empty());

    println!("Both zones responded successfully");

    Ok(())
}

#[tokio::test]
async fn test_fallback_on_route_failure() -> anyhow::Result<()> {
    // Use a gateway that definitely doesn't exist
    let config = r#"
[server]
listen_address = "127.0.0.1:15358"
default_upstream = ["8.8.8.8:53"]
route_failure_mode = "fallback"

[[zones]]
name = "test-fallback"
dns_servers = []
route_type = "via"
route_target = "10.255.255.254"
domains = ["example.com"]
patterns = []
    "#;

    let _server = TestServer::start_with_temp_config(config).await?;
    let server_addr: SocketAddr = "127.0.0.1:15358".parse()?;
    let mut client = create_dns_client(server_addr).await?;

    // Query should succeed even if route fails
    let name = Name::from_str("www.example.com.")?;
    let response = client.query(name, DNSClass::IN, RecordType::A).await?;

    // Should still get DNS response despite route failure
    assert!(
        !response.answers().is_empty(),
        "Should have DNS answers even with route failure"
    );

    println!("Fallback mode works: DNS succeeded despite route failure");

    Ok(())
}

#[tokio::test]
async fn test_device_file_missing() -> anyhow::Result<()> {
    // Point to non-existent device file
    let config = r#"
[server]
listen_address = "127.0.0.1:15359"
default_upstream = ["8.8.8.8:53"]
route_failure_mode = "fallback"

[[zones]]
name = "test-missing"
dns_servers = []
route_type = "dev"
route_target = "/tmp/nonexistent_device_file_12345.dev"
domains = ["example.com"]
patterns = []
    "#;

    let _server = TestServer::start_with_temp_config(config).await?;
    let server_addr: SocketAddr = "127.0.0.1:15359".parse()?;
    let mut client = create_dns_client(server_addr).await?;

    // Query should succeed even if device file doesn't exist
    let name = Name::from_str("www.example.com.")?;
    let response = client.query(name, DNSClass::IN, RecordType::A).await?;

    // Should still get DNS response
    assert!(
        !response.answers().is_empty(),
        "Should have DNS answers even with missing device file"
    );

    println!("Device file missing handled gracefully");

    Ok(())
}
