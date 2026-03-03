mod aggregator;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use crate::config::{RouteType, ZoneConfig, ZoneMode};
use aggregator::{RouteAction, RouteAggregator};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Clone)]
struct ExcludedRange {
    network: u32,
    prefix_len: u8,
}

impl ExcludedRange {
    fn contains_v4(&self, ip: Ipv4Addr) -> bool {
        if self.prefix_len == 0 {
            return true;
        }
        let mask = !((1u32 << (32 - self.prefix_len)) - 1);
        (u32::from(ip) & mask) == self.network
    }
}

#[cfg(target_os = "linux")]
use linux::LinuxRouteAdder as PlatformRouteAdder;
#[cfg(target_os = "macos")]
use macos::MacosRouteAdder as PlatformRouteAdder;

#[async_trait]
pub(crate) trait RouteAdder: Send + Sync {
    async fn add_via_route(&self, ip: IpAddr, prefix_len: u8, gateway: &str) -> Result<()>;
    async fn add_dev_route(&self, ip: IpAddr, prefix_len: u8, device: &str) -> Result<()>;
    async fn remove_route(&self, ip: IpAddr, prefix_len: u8) -> Result<()>;
}

pub struct RouteManager {
    adder: PlatformRouteAdder,
    zone_routes: Arc<RwLock<HashMap<String, HashSet<IpAddr>>>>,
    aggregator: Mutex<RouteAggregator>,
    excluded_ranges: Vec<ExcludedRange>,
}

impl RouteManager {
    pub fn new(aggregation_prefix: Option<u8>, zones: &[ZoneConfig]) -> Result<Self> {
        let adder = PlatformRouteAdder::new()?;

        // Build excluded ranges from exclusive zones' static_routes
        let mut excluded_ranges = Vec::new();
        for zone in zones {
            if zone.mode == ZoneMode::Exclusive {
                for cidr in &zone.static_routes {
                    match parse_cidr(cidr) {
                        Ok((IpAddr::V4(ip), prefix_len)) => {
                            excluded_ranges.push(ExcludedRange {
                                network: u32::from(ip) & (!((1u32 << (32 - prefix_len)) - 1)),
                                prefix_len,
                            });
                        }
                        Ok((IpAddr::V6(_), _)) => {
                            tracing::warn!(
                                cidr = cidr,
                                zone = zone.name,
                                "IPv6 CIDR in exclusive zone static_routes is not supported for exclusion, skipping"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                cidr = cidr,
                                zone = zone.name,
                                error = %e,
                                "Failed to parse CIDR in exclusive zone static_routes, skipping"
                            );
                        }
                    }
                }
            }
        }

        Ok(Self {
            adder,
            zone_routes: Arc::new(RwLock::new(HashMap::new())),
            aggregator: Mutex::new(RouteAggregator::new(aggregation_prefix)),
            excluded_ranges,
        })
    }

    /// Add a route for the given IP based on zone configuration.
    /// For IPv4 with aggregation enabled, installs a wider CIDR prefix.
    /// For IPv6, always uses /128 (no aggregation).
    pub async fn add_route(&self, ip: IpAddr, zone: &ZoneConfig) -> Result<()> {
        // Skip route installation if IP falls within an excluded range
        if self.is_excluded(ip) {
            tracing::debug!(
                ip = %ip,
                zone = zone.name,
                "IP is in excluded range, skipping route installation"
            );
            return Ok(());
        }

        match ip {
            IpAddr::V4(v4) => self.add_route_v4(v4, zone).await,
            IpAddr::V6(_) => self.add_route_simple(ip, 128, zone).await,
        }
    }

    async fn add_route_v4(&self, ip: Ipv4Addr, zone: &ZoneConfig) -> Result<()> {
        let actions = {
            let mut agg = self.aggregator.lock().await;
            agg.process_ip(ip, &zone.name, zone.route_type, &zone.route_target)
        };

        if actions.is_empty() {
            return Ok(());
        }

        for action in &actions {
            self.execute_action(action).await?;
        }

        let mut routes = self.zone_routes.write().await;
        routes
            .entry(zone.name.clone())
            .or_default()
            .insert(IpAddr::V4(ip));

        Ok(())
    }

    /// Execute a single RouteAction against the kernel.
    async fn execute_action(&self, action: &RouteAction) -> Result<()> {
        match action {
            RouteAction::Add {
                network,
                prefix_len,
                route_type,
                route_target,
            } => {
                let ip = IpAddr::V4(*network);
                match route_type {
                    RouteType::Via => {
                        self.adder
                            .add_via_route(ip, *prefix_len, route_target)
                            .await
                    }
                    RouteType::Dev => {
                        let device = self.read_device_file(route_target).await?;
                        self.adder.add_dev_route(ip, *prefix_len, &device).await
                    }
                }
            }
            RouteAction::Remove {
                network,
                prefix_len,
            } => {
                self.adder
                    .remove_route(IpAddr::V4(*network), *prefix_len)
                    .await
            }
        }
    }

    /// Simple route add without aggregation (used for IPv6).
    async fn add_route_simple(&self, ip: IpAddr, prefix_len: u8, zone: &ZoneConfig) -> Result<()> {
        let result = match zone.route_type {
            RouteType::Via => {
                self.adder
                    .add_via_route(ip, prefix_len, &zone.route_target)
                    .await
            }
            RouteType::Dev => {
                let device = self.read_device_file(&zone.route_target).await?;
                self.adder.add_dev_route(ip, prefix_len, &device).await
            }
        };

        if result.is_ok() {
            let mut routes = self.zone_routes.write().await;
            routes.entry(zone.name.clone()).or_default().insert(ip);
        }

        result
    }

    /// Add a static route from a CIDR string (e.g. "149.154.160.0/20" or "1.2.3.4").
    /// Static routes bypass aggregation but register their IPs so aggregates don't overlap.
    pub async fn add_static_route(&self, cidr: &str, zone: &ZoneConfig) -> Result<()> {
        let (ip, prefix_len) = parse_cidr(cidr)?;

        tracing::info!(cidr = cidr, zone = zone.name, "Adding static route");

        // Register individual IPs in the aggregator so future aggregates
        // don't accidentally cover them
        if let IpAddr::V4(v4) = ip {
            let mut agg = self.aggregator.lock().await;
            agg.register_static_ip(v4, &zone.name);
        }

        let result = match zone.route_type {
            RouteType::Via => {
                self.adder
                    .add_via_route(ip, prefix_len, &zone.route_target)
                    .await
            }
            RouteType::Dev => {
                let device = self.read_device_file(&zone.route_target).await?;
                self.adder.add_dev_route(ip, prefix_len, &device).await
            }
        };

        if result.is_ok() {
            let mut routes = self.zone_routes.write().await;
            routes.entry(zone.name.clone()).or_default().insert(ip);
        }

        result
    }

    async fn read_device_file(&self, path: &str) -> Result<String> {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => {
                let device = content.trim().to_string();
                if device.is_empty() {
                    anyhow::bail!("Device file '{path}' is empty");
                }
                Ok(device)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                anyhow::bail!("Device file '{path}' not found (VPN not connected?)");
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Clean up routes for a specific zone
    ///
    /// Removes the zone from tracking but does NOT delete routes from the
    /// kernel routing table. Routes will naturally expire or be replaced.
    pub async fn cleanup_zone(&self, zone_name: &str) -> Result<()> {
        let mut routes = self.zone_routes.write().await;

        if let Some(ips) = routes.remove(zone_name) {
            tracing::info!(
                zone = zone_name,
                route_count = ips.len(),
                "Removed zone from tracking (routes remain in kernel table)"
            );
            tracing::debug!(
                zone = zone_name,
                ips = ?ips,
                "Routes that were tracked for this zone"
            );
        } else {
            tracing::debug!(zone = zone_name, "Zone has no tracked routes");
        }

        // Also clean up aggregator state
        let mut agg = self.aggregator.lock().await;
        agg.cleanup_zone(zone_name);

        Ok(())
    }

    /// Get count of tracked routes for a zone
    #[allow(dead_code)]
    pub async fn get_zone_route_count(&self, zone_name: &str) -> usize {
        let routes = self.zone_routes.read().await;
        routes.get(zone_name).map(|set| set.len()).unwrap_or(0)
    }

    /// Check if an IP falls within any excluded range.
    /// Returns true if IPv4 and matches any excluded_ranges entry; always false for IPv6.
    fn is_excluded(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => self.excluded_ranges.iter().any(|r| r.contains_v4(v4)),
            IpAddr::V6(_) => false,
        }
    }
}

/// Parse a CIDR string like "149.154.160.0/20" or plain IP "1.2.3.4"
fn parse_cidr(cidr: &str) -> Result<(IpAddr, u8)> {
    if let Some((ip_str, prefix_str)) = cidr.split_once('/') {
        let ip: IpAddr = ip_str.parse().context("Failed to parse IP in CIDR")?;
        let prefix_len: u8 = prefix_str
            .parse()
            .context("Failed to parse prefix length")?;
        let max = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        if prefix_len > max {
            anyhow::bail!("Prefix length {prefix_len} exceeds maximum {max} for {ip}");
        }
        Ok((ip, prefix_len))
    } else {
        let ip: IpAddr = cidr.parse().context("Failed to parse IP")?;
        let prefix_len = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        Ok((ip, prefix_len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_range_contains_host_in_prefix() {
        let range = ExcludedRange {
            network: u32::from(Ipv4Addr::new(10, 0, 0, 0)),
            prefix_len: 8,
        };
        assert!(range.contains_v4(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(range.contains_v4(Ipv4Addr::new(10, 255, 255, 255)));
        assert!(!range.contains_v4(Ipv4Addr::new(11, 0, 0, 0)));
        assert!(!range.contains_v4(Ipv4Addr::new(192, 168, 1, 1)));
    }

    #[test]
    fn excluded_range_rfc1918() {
        let range = ExcludedRange {
            network: u32::from(Ipv4Addr::new(192, 168, 0, 0)),
            prefix_len: 16,
        };
        assert!(range.contains_v4(Ipv4Addr::new(192, 168, 0, 0)));
        assert!(range.contains_v4(Ipv4Addr::new(192, 168, 1, 100)));
        assert!(range.contains_v4(Ipv4Addr::new(192, 168, 255, 255)));
        assert!(!range.contains_v4(Ipv4Addr::new(192, 169, 0, 0)));
        assert!(!range.contains_v4(Ipv4Addr::new(10, 0, 0, 1)));
    }

    #[test]
    fn excluded_range_host_32() {
        let range = ExcludedRange {
            network: u32::from(Ipv4Addr::new(1, 2, 3, 4)),
            prefix_len: 32,
        };
        assert!(range.contains_v4(Ipv4Addr::new(1, 2, 3, 4)));
        assert!(!range.contains_v4(Ipv4Addr::new(1, 2, 3, 5)));
        assert!(!range.contains_v4(Ipv4Addr::new(1, 2, 3, 3)));
    }

    #[test]
    fn excluded_range_zero_prefix_matches_all() {
        let range = ExcludedRange {
            network: 0,
            prefix_len: 0,
        };
        assert!(range.contains_v4(Ipv4Addr::new(0, 0, 0, 0)));
        assert!(range.contains_v4(Ipv4Addr::new(255, 255, 255, 255)));
        assert!(range.contains_v4(Ipv4Addr::new(192, 168, 1, 1)));
    }

    #[tokio::test]
    async fn build_excluded_ranges_from_exclusive_zones() {
        use crate::config::{RouteType, ZoneConfig};

        let zones = vec![
            ZoneConfig {
                name: "vpn-catchall".to_string(),
                mode: ZoneMode::Exclusive,
                dns_servers: vec![],
                dns_protocol: crate::config::DnsProtocol::Udp,
                route_type: RouteType::Via,
                route_target: "10.8.0.1".to_string(),
                domains: vec![],
                patterns: vec![],
                static_routes: vec!["10.0.0.0/8".to_string(), "192.168.0.0/16".to_string()],
                cache_min_ttl: None,
                cache_max_ttl: None,
                cache_negative_ttl: None,
            },
            ZoneConfig {
                name: "normal-zone".to_string(),
                mode: ZoneMode::Inclusive,
                dns_servers: vec![],
                dns_protocol: crate::config::DnsProtocol::Udp,
                route_type: RouteType::Via,
                route_target: "10.8.0.1".to_string(),
                domains: vec![],
                patterns: vec![],
                static_routes: vec!["172.16.0.0/12".to_string()],
                cache_min_ttl: None,
                cache_max_ttl: None,
                cache_negative_ttl: None,
            },
        ];

        let rm = RouteManager::new(None, &zones).unwrap();

        // Exclusive zone's static_routes should be excluded
        assert!(rm.is_excluded(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(rm.is_excluded(IpAddr::V4(Ipv4Addr::new(10, 255, 255, 255))));
        assert!(rm.is_excluded(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));

        // Inclusive zone's static_routes should NOT be excluded
        assert!(!rm.is_excluded(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(!rm.is_excluded(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));

        // IPv6 never excluded
        assert!(!rm.is_excluded(IpAddr::V6("::1".parse().unwrap())));
    }
}
