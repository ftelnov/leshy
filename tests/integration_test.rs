use hickory_client::client::{AsyncClient, ClientHandle};
use hickory_client::rr::{DNSClass, Name, RecordType};
use hickory_client::udp::UdpClientStream;
use std::net::SocketAddr;
use std::str::FromStr;
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::time::{sleep, Duration};

struct TestServer {
    _process: Child,
    _temp_dir: Option<TempDir>,
}

impl TestServer {
    #[allow(dead_code)]
    async fn start(config_path: &str) -> anyhow::Result<Self> {
        // Build the server binary
        let status = Command::new("cargo")
            .args(["build", "--release"])
            .status()
            .await?;

        if !status.success() {
            anyhow::bail!("Failed to build leshy");
        }

        // Start the server
        let mut process = Command::new("target/release/leshy")
            .arg(config_path)
            .env("RUST_LOG", "info")
            .spawn()?;

        // Give server time to start
        sleep(Duration::from_millis(500)).await;

        // Check if process is still running
        match process.try_wait() {
            Ok(Some(status)) => {
                anyhow::bail!("Server exited immediately with status: {status}");
            }
            Ok(None) => {
                // Still running, good
            }
            Err(e) => {
                anyhow::bail!("Error checking server status: {e}");
            }
        }

        Ok(Self {
            _process: process,
            _temp_dir: None,
        })
    }

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

    // Run the background task
    tokio::spawn(bg);

    Ok(client)
}

#[tokio::test]
async fn test_dns_query_and_response() -> anyhow::Result<()> {
    // Start server with temporary config
    let config = r#"
[server]
listen_address = "127.0.0.1:15370"
default_upstream = ["8.8.8.8:53"]
route_failure_mode = "fallback"

[[zones]]
name = "test-zone"
dns_servers = []
route_type = "via"
route_target = "192.168.100.1"
domains = ["example.com"]
patterns = ["test"]
    "#;

    let _server = TestServer::start_with_temp_config(config).await?;

    // Create DNS client
    let server_addr: SocketAddr = "127.0.0.1:15370".parse()?;
    let mut client = create_dns_client(server_addr).await?;

    // Query a domain
    let name = Name::from_str("www.google.com.")?;
    let response = client.query(name, DNSClass::IN, RecordType::A).await?;

    // Verify we got answers
    assert!(!response.answers().is_empty(), "Should have DNS answers");

    println!(
        "DNS query successful! Got {} answers",
        response.answers().len()
    );

    Ok(())
}

#[tokio::test]
async fn test_zone_matching() -> anyhow::Result<()> {
    let config = r#"
[server]
listen_address = "127.0.0.1:15354"
default_upstream = ["8.8.8.8:53"]
route_failure_mode = "fallback"

[[zones]]
name = "test-exact"
dns_servers = []
route_type = "via"
route_target = "192.168.100.1"
domains = ["example.com"]
patterns = []

[[zones]]
name = "test-pattern"
dns_servers = []
route_type = "via"
route_target = "192.168.100.2"
domains = []
patterns = ["github"]
    "#;

    let _server = TestServer::start_with_temp_config(config).await?;

    // Create DNS client
    let server_addr: SocketAddr = "127.0.0.1:15354".parse()?;
    let mut client = create_dns_client(server_addr).await?;

    // Query domains that should match zones
    let name1 = Name::from_str("www.example.com.")?;
    let response1 = client.query(name1, DNSClass::IN, RecordType::A).await?;
    assert!(!response1.answers().is_empty());

    let name2 = Name::from_str("github.com.")?;
    let response2 = client.query(name2, DNSClass::IN, RecordType::A).await?;
    assert!(!response2.answers().is_empty());

    println!("Zone matching test passed!");

    Ok(())
}

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
