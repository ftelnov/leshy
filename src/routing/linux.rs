use super::RouteAdder;
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::TryStreamExt;
use netlink_packet_route::route::{RouteAddress, RouteScope};
use rtnetlink::{new_connection, Handle};
use std::net::IpAddr;

pub struct LinuxRouteAdder {
    handle: Handle,
}

impl LinuxRouteAdder {
    pub fn new() -> Result<Self> {
        let (connection, handle, _) = new_connection()?;
        tokio::spawn(connection);
        Ok(Self { handle })
    }
}

#[async_trait]
impl RouteAdder for LinuxRouteAdder {
    async fn add_via_route(&self, ip: IpAddr, gateway: &str) -> Result<()> {
        let gateway_ip: IpAddr = gateway.parse().context("Failed to parse gateway IP")?;

        tracing::info!(ip = %ip, gateway = %gateway, "Adding route via gateway");

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
            Err(rtnetlink::Error::NetlinkError(err))
                if matches!(err.code, Some(code) if code.get() == -17) =>
            {
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
        tracing::info!(ip = %ip, device = device, "Adding route via device");

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
            Err(rtnetlink::Error::NetlinkError(err))
                if matches!(err.code, Some(code) if code.get() == -17) =>
            {
                tracing::debug!(ip = %ip, "Route already exists");
                Ok(())
            }
            Err(e) => {
                tracing::error!(ip = %ip, error = %e, "Failed to add route");
                Err(e.into())
            }
        }
    }
}
