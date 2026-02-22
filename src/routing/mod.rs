mod aggregator;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use crate::config::{RouteType, ZoneConfig};
use aggregator::{RouteAction, RouteAggregator};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

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
}

impl RouteManager {
    pub fn new(aggregation_prefix: Option<u8>) -> Result<Self> {
        let adder = PlatformRouteAdder::new()?;
        Ok(Self {
            adder,
            zone_routes: Arc::new(RwLock::new(HashMap::new())),
            aggregator: Mutex::new(RouteAggregator::new(aggregation_prefix)),
        })
    }

    /// Add a route for the given IP based on zone configuration.
    /// For IPv4 with aggregation enabled, installs a wider CIDR prefix.
    /// For IPv6, always uses /128 (no aggregation).
    pub async fn add_route(&self, ip: IpAddr, zone: &ZoneConfig) -> Result<()> {
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
