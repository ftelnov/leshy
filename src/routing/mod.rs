#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use crate::config::{RouteType, ZoneConfig};
use anyhow::Result;
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
    async fn add_via_route(&self, ip: IpAddr, gateway: &str) -> Result<()>;
    async fn add_dev_route(&self, ip: IpAddr, device: &str) -> Result<()>;
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

    /// Add a route for the given IP based on zone configuration
    pub async fn add_route(&self, ip: IpAddr, zone: &ZoneConfig) -> Result<()> {
        let result = match zone.route_type {
            RouteType::Via => self.adder.add_via_route(ip, &zone.route_target).await,
            RouteType::Dev => {
                let device = self.read_device_file(&zone.route_target).await?;
                self.adder.add_dev_route(ip, &device).await
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
