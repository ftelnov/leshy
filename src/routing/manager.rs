use crate::config::{RouteType, ZoneConfig};
use anyhow::{Context, Result};
use futures::TryStreamExt;
use netlink_packet_route::route::{RouteAddress, RouteScope};
use rtnetlink::{new_connection, Handle};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct RouteManager {
    handle: Handle,
    /// Track which IPs were added for each zone
    zone_routes: Arc<RwLock<HashMap<String, HashSet<IpAddr>>>>,
}

impl RouteManager {
    pub fn new() -> Result<Self> {
        let (connection, handle, _) = new_connection()?;

        // Spawn the connection in the background
        tokio::spawn(connection);

        Ok(Self {
            handle,
            zone_routes: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Add a route for the given IP based on zone configuration
    pub async fn add_route(&self, ip: IpAddr, zone: &ZoneConfig) -> Result<()> {
        let result = match zone.route_type {
            RouteType::Via => self.add_via_route(ip, &zone.route_target).await,
            RouteType::Dev => {
                // Read device name from file
                let device = self.read_device_file(&zone.route_target).await?;
                self.add_dev_route(ip, &device).await
            }
        };

        // Track the route if successful
        if result.is_ok() {
            let mut routes = self.zone_routes.write().await;
            routes
                .entry(zone.name.clone())
                .or_insert_with(HashSet::new)
                .insert(ip);
        }

        result
    }

    async fn add_via_route(&self, ip: IpAddr, gateway: &str) -> Result<()> {
        let gateway_ip: IpAddr = gateway.parse().context("Failed to parse gateway IP")?;

        tracing::info!(
            ip = %ip,
            gateway = %gateway,
            "Adding route via gateway"
        );

        let route = match ip {
            IpAddr::V4(addr) => {
                let mut route = self.handle.route().add().v4();
                route.message_mut().header.destination_prefix_length = 32;
                route.message_mut().attributes.push(
                    netlink_packet_route::route::RouteAttribute::Destination(RouteAddress::Inet(
                        addr,
                    )),
                );

                if let IpAddr::V4(gw) = gateway_ip {
                    route.message_mut().attributes.push(
                        netlink_packet_route::route::RouteAttribute::Gateway(RouteAddress::Inet(
                            gw,
                        )),
                    );
                }

                route.message_mut().header.scope = RouteScope::Universe;
                route.execute().await
            }
            IpAddr::V6(addr) => {
                let mut route = self.handle.route().add().v6();
                route.message_mut().header.destination_prefix_length = 128;
                route.message_mut().attributes.push(
                    netlink_packet_route::route::RouteAttribute::Destination(RouteAddress::Inet6(
                        addr,
                    )),
                );

                if let IpAddr::V6(gw) = gateway_ip {
                    route.message_mut().attributes.push(
                        netlink_packet_route::route::RouteAttribute::Gateway(RouteAddress::Inet6(
                            gw,
                        )),
                    );
                }

                route.message_mut().header.scope = RouteScope::Universe;
                route.execute().await
            }
        };

        match route {
            Ok(_) => {
                tracing::debug!(ip = %ip, gateway = %gateway, "Route added successfully");
                Ok(())
            }
            Err(rtnetlink::Error::NetlinkError(err)) if matches!(err.code, Some(code) if code.get() == -17) =>
            {
                // EEXIST - route already exists, this is fine
                tracing::debug!(ip = %ip, "Route already exists");
                Ok(())
            }
            Err(e) => {
                tracing::error!(ip = %ip, error = %e, "Failed to add route");
                Err(e.into())
            }
        }
    }

    async fn add_dev_route(&self, ip: IpAddr, device: &str) -> Result<()> {
        tracing::info!(
            ip = %ip,
            device = device,
            "Adding route via device"
        );

        // Get interface index
        let mut links = self
            .handle
            .link()
            .get()
            .match_name(device.to_string())
            .execute();
        let link = links
            .try_next()
            .await?
            .context(format!("Device '{device}' not found"))?;

        let route = match ip {
            IpAddr::V4(addr) => {
                let mut route = self.handle.route().add().v4();
                route.message_mut().header.destination_prefix_length = 32;
                route.message_mut().attributes.push(
                    netlink_packet_route::route::RouteAttribute::Destination(RouteAddress::Inet(
                        addr,
                    )),
                );
                route.message_mut().attributes.push(
                    netlink_packet_route::route::RouteAttribute::Oif(link.header.index),
                );
                route.message_mut().header.scope = RouteScope::Link;
                route.execute().await
            }
            IpAddr::V6(addr) => {
                let mut route = self.handle.route().add().v6();
                route.message_mut().header.destination_prefix_length = 128;
                route.message_mut().attributes.push(
                    netlink_packet_route::route::RouteAttribute::Destination(RouteAddress::Inet6(
                        addr,
                    )),
                );
                route.message_mut().attributes.push(
                    netlink_packet_route::route::RouteAttribute::Oif(link.header.index),
                );
                route.message_mut().header.scope = RouteScope::Link;
                route.execute().await
            }
        };

        match route {
            Ok(_) => {
                tracing::debug!(ip = %ip, device = device, "Route added successfully");
                Ok(())
            }
            Err(rtnetlink::Error::NetlinkError(err)) if matches!(err.code, Some(code) if code.get() == -17) =>
            {
                // EEXIST - route already exists, this is fine
                tracing::debug!(ip = %ip, "Route already exists");
                Ok(())
            }
            Err(e) => {
                tracing::error!(ip = %ip, error = %e, "Failed to add route");
                Err(e.into())
            }
        }
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
    /// Note: This removes the zone from tracking but does NOT delete the actual routes from
    /// the kernel routing table. Deleting routes can be dangerous and may break active connections.
    /// Routes will naturally expire or be replaced when the system configuration changes.
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
