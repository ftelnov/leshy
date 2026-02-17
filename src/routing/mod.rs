#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use crate::config::{RouteType, ZoneConfig};
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

#[cfg(target_os = "linux")]
use linux::LinuxRouteAdder as PlatformRouteAdder;
#[cfg(target_os = "macos")]
use macos::MacosRouteAdder as PlatformRouteAdder;

#[async_trait]
pub(crate) trait RouteAdder: Send + Sync {
    async fn add_via_route(&self, ip: IpAddr, prefix_len: u8, gateway: &str) -> Result<()>;
    async fn add_dev_route(&self, ip: IpAddr, prefix_len: u8, device: &str) -> Result<()>;
}

pub struct RouteManager {
    adder: PlatformRouteAdder,
    zone_routes: Arc<RwLock<HashMap<String, HashSet<IpAddr>>>>,
}

impl RouteManager {
    pub fn new() -> Result<Self> {
        let adder = PlatformRouteAdder::new()?;
        Ok(Self {
            adder,
            zone_routes: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Add a route for the given IP based on zone configuration (uses /32 or /128 prefix)
    pub async fn add_route(&self, ip: IpAddr, zone: &ZoneConfig) -> Result<()> {
        let prefix_len = match ip {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };

        let result = match zone.route_type {
            RouteType::Via => self.adder.add_via_route(ip, prefix_len, &zone.route_target).await,
            RouteType::Dev => {
                let device = self.read_device_file(&zone.route_target).await?;
                self.adder.add_dev_route(ip, prefix_len, &device).await
            }
        };

        if result.is_ok() {
            let mut routes = self.zone_routes.write().await;
            routes
                .entry(zone.name.clone())
                .or_default()
                .insert(ip);
        }

        result
    }

    /// Add a static route from a CIDR string (e.g. "149.154.160.0/20" or "1.2.3.4")
    pub async fn add_static_route(&self, cidr: &str, zone: &ZoneConfig) -> Result<()> {
        let (ip, prefix_len) = parse_cidr(cidr)?;

        tracing::info!(cidr = cidr, zone = zone.name, "Adding static route");

        let result = match zone.route_type {
            RouteType::Via => self.adder.add_via_route(ip, prefix_len, &zone.route_target).await,
            RouteType::Dev => {
                let device = self.read_device_file(&zone.route_target).await?;
                self.adder.add_dev_route(ip, prefix_len, &device).await
            }
        };

        if result.is_ok() {
            let mut routes = self.zone_routes.write().await;
            routes
                .entry(zone.name.clone())
                .or_default()
                .insert(ip);
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
        let prefix_len: u8 = prefix_str.parse().context("Failed to parse prefix length")?;
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
