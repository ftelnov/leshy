use serde::{Deserialize, Deserializer, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub zones: Vec<ZoneConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    pub listen_address: SocketAddr,
    pub default_upstream: Vec<SocketAddr>,

    /// What to do when route addition fails:
    /// - "servfail": Return SERVFAIL to client
    /// - "fallback": Continue and return DNS response (default)
    #[serde(default = "default_route_failure_mode")]
    pub route_failure_mode: RouteFailureMode,

    /// Enable automatic config reload when file changes
    #[serde(default)]
    pub auto_reload: bool,

    /// Directory to load additional zone configs from.
    /// Defaults to config.d/ next to the main config file.
    #[serde(default)]
    pub config_dir: Option<String>,

    /// Maximum number of cache entries (0 = disabled)
    #[serde(default = "default_cache_size")]
    pub cache_size: usize,

    /// Minimum TTL for cached responses (seconds)
    #[serde(default = "default_cache_min_ttl")]
    pub cache_min_ttl: u64,

    /// Maximum TTL for cached responses (seconds)
    #[serde(default = "default_cache_max_ttl")]
    pub cache_max_ttl: u64,

    /// TTL for NXDOMAIN / empty responses (seconds)
    #[serde(default = "default_cache_negative_ttl")]
    pub cache_negative_ttl: u64,

    /// CIDR prefix length for route aggregation (e.g. 22 = /22, 1024 IPs).
    /// When set, DNS-resolved IPv4 addresses are grouped into wider subnets
    /// to reduce the number of kernel routes. Unset or 32 = disabled.
    #[serde(default)]
    pub route_aggregation_prefix: Option<u8>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RouteFailureMode {
    Servfail,
    Fallback,
}

fn default_route_failure_mode() -> RouteFailureMode {
    RouteFailureMode::Fallback
}

fn default_cache_size() -> usize {
    1000
}
fn default_cache_min_ttl() -> u64 {
    60
}
fn default_cache_max_ttl() -> u64 {
    3600
}
fn default_cache_negative_ttl() -> u64 {
    30
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ZoneConfig {
    pub name: String,

    /// Zone matching mode: "inclusive" (default) or "exclusive"
    /// Inclusive: matches only listed domains/patterns
    /// Exclusive: matches everything EXCEPT listed domains/patterns
    #[serde(default)]
    pub mode: ZoneMode,

    /// DNS servers for this zone. Empty = use default upstream.
    /// Supports both simple format: ["10.44.2.2:53"]
    /// and rich format: [{ address = "10.44.2.2:53", cache_min_ttl = 10 }]
    #[serde(default, deserialize_with = "deserialize_dns_servers")]
    pub dns_servers: Vec<DnsServerConfig>,

    /// How to route resolved IPs
    pub route_type: RouteType,

    /// For "via": gateway IP address
    /// For "dev": path to device file
    pub route_target: String,

    /// Exact domain matches (domain + all subdomains)
    #[serde(default)]
    pub domains: Vec<String>,

    /// Substring pattern matches
    #[serde(default)]
    pub patterns: Vec<String>,

    /// Static IP/CIDR routes to add on startup (e.g. "149.154.160.0/20", "1.2.3.4")
    #[serde(default)]
    pub static_routes: Vec<String>,

    /// Protocol for upstream DNS queries: "udp" (default) or "tcp".
    /// Use "tcp" when upstream is reachable only through a SOCKS5/TCP proxy (e.g. tun2socks).
    #[serde(default)]
    pub dns_protocol: DnsProtocol,

    /// Per-zone cache minimum TTL override (seconds)
    #[serde(default)]
    pub cache_min_ttl: Option<u64>,

    /// Per-zone cache maximum TTL override (seconds)
    #[serde(default)]
    pub cache_max_ttl: Option<u64>,

    /// Per-zone negative TTL override (seconds)
    #[serde(default)]
    pub cache_negative_ttl: Option<u64>,
}

/// Per-server DNS configuration with optional cache TTL overrides.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DnsServerConfig {
    pub address: SocketAddr,
    #[serde(default)]
    pub cache_min_ttl: Option<u64>,
    #[serde(default)]
    pub cache_max_ttl: Option<u64>,
    #[serde(default)]
    pub cache_negative_ttl: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum DnsServerEntry {
    Simple(SocketAddr),
    Rich(DnsServerConfig),
}

fn deserialize_dns_servers<'de, D>(deserializer: D) -> Result<Vec<DnsServerConfig>, D::Error>
where
    D: Deserializer<'de>,
{
    let entries: Vec<DnsServerEntry> = Vec::deserialize(deserializer)?;
    Ok(entries
        .into_iter()
        .map(|entry| match entry {
            DnsServerEntry::Simple(address) => DnsServerConfig {
                address,
                cache_min_ttl: None,
                cache_max_ttl: None,
                cache_negative_ttl: None,
            },
            DnsServerEntry::Rich(config) => config,
        })
        .collect())
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DnsProtocol {
    #[default]
    Udp,
    Tcp,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ZoneMode {
    /// Match only listed domains/patterns (default)
    #[default]
    Inclusive,
    /// Match everything EXCEPT listed domains/patterns
    Exclusive,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RouteType {
    /// Static gateway IP
    Via,
    /// Dynamic device from file
    Dev,
}

impl Config {
    pub fn from_file(path: &PathBuf) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        config.validate()?;
        Ok(config)
    }

    /// Load config from main file and merge with config.d directory
    ///
    /// Main config file contains server settings.
    /// config.d directory contains zone definitions (*.toml files).
    /// All zones are merged together.
    pub fn from_file_with_includes(path: &PathBuf) -> anyhow::Result<Self> {
        // Load main config
        let mut config = Self::from_file(path)?;

        // Use explicit config_dir if set, otherwise look next to main config
        let config_dir = if let Some(ref dir) = config.server.config_dir {
            PathBuf::from(dir)
        } else {
            path.parent()
                .map(|p| p.join("config.d"))
                .unwrap_or_else(|| PathBuf::from("config.d"))
        };

        if config_dir.exists() && config_dir.is_dir() {
            tracing::info!(dir = %config_dir.display(), "Loading additional configs from directory");

            // Load all .toml files from config.d
            let mut entries: Vec<_> = std::fs::read_dir(&config_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s == "toml")
                        .unwrap_or(false)
                })
                .collect();

            // Sort by filename for predictable ordering
            entries.sort_by_key(|e| e.file_name());

            for entry in entries {
                let zone_file = entry.path();
                match Self::load_zones_from_file(&zone_file) {
                    Ok(zones) => {
                        tracing::info!(
                            file = %zone_file.display(),
                            zone_count = zones.len(),
                            "Loaded zones from file"
                        );
                        config.zones.extend(zones);
                    }
                    Err(e) => {
                        tracing::warn!(
                            file = %zone_file.display(),
                            error = %e,
                            "Failed to load zone file, skipping"
                        );
                    }
                }
            }
        }

        config.validate()?;
        Ok(config)
    }

    /// Load only zones from a config file (ignore server settings)
    fn load_zones_from_file(path: &PathBuf) -> anyhow::Result<Vec<ZoneConfig>> {
        let content = std::fs::read_to_string(path)?;

        // Try to parse as full config (for compatibility)
        if let Ok(config) = toml::from_str::<Config>(&content) {
            return Ok(config.zones);
        }

        // Try to parse as zones-only config
        #[derive(Deserialize)]
        struct ZonesOnly {
            zones: Vec<ZoneConfig>,
        }

        if let Ok(zones_only) = toml::from_str::<ZonesOnly>(&content) {
            return Ok(zones_only.zones);
        }

        anyhow::bail!("Could not parse zones from file");
    }

    fn validate(&self) -> anyhow::Result<()> {
        // Validate listen address is not 0.0.0.0:0
        if self.server.listen_address.port() == 0 {
            anyhow::bail!("Server listen port cannot be 0");
        }

        // Validate default upstream not empty
        if self.server.default_upstream.is_empty() {
            anyhow::bail!("default_upstream cannot be empty");
        }

        // Validate zones
        for zone in &self.zones {
            if zone.mode == ZoneMode::Inclusive
                && zone.domains.is_empty()
                && zone.patterns.is_empty()
                && zone.static_routes.is_empty()
            {
                anyhow::bail!(
                    "Zone '{}' must have at least one domain, pattern, or static route",
                    zone.name
                );
            }

            // Validate pattern regexes
            for pattern in &zone.patterns {
                if let Err(e) = regex::Regex::new(pattern) {
                    anyhow::bail!(
                        "Zone '{}': invalid regex pattern '{}': {}",
                        zone.name,
                        pattern,
                        e
                    );
                }
            }
        }

        // Validate route_aggregation_prefix
        if let Some(prefix) = self.server.route_aggregation_prefix {
            if !(8..=32).contains(&prefix) {
                anyhow::bail!("route_aggregation_prefix must be between 8 and 32, got {prefix}");
            }
        }

        // Check for duplicate zone names
        let mut seen = std::collections::HashSet::new();
        for zone in &self.zones {
            if !seen.insert(&zone.name) {
                anyhow::bail!("Duplicate zone name: '{}'", zone.name);
            }
        }

        Ok(())
    }
}
