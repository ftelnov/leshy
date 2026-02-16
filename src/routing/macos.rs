use super::RouteAdder;
use anyhow::Result;
use async_trait::async_trait;
use std::net::IpAddr;
use tokio::process::Command;

pub struct MacosRouteAdder;

impl MacosRouteAdder {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

#[async_trait]
impl RouteAdder for MacosRouteAdder {
    async fn add_via_route(&self, ip: IpAddr, gateway: &str) -> Result<()> {
        tracing::info!(ip = %ip, gateway = %gateway, "Adding route via gateway");

        let mut args = vec!["-n", "add"];
        if ip.is_ipv6() {
            args.push("-inet6");
        }
        let ip_str = ip.to_string();
        args.extend(["-host", &ip_str, gateway]);

        let output = Command::new("/sbin/route")
            .args(&args)
            .output()
            .await?;

        if output.status.success() {
            tracing::debug!(ip = %ip, gateway = %gateway, "Route added successfully");
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // "File exists" = route already present, not an error
            if stderr.contains("File exists") {
                tracing::debug!(ip = %ip, "Route already exists");
                Ok(())
            } else {
                tracing::error!(ip = %ip, stderr = %stderr, "Failed to add route");
                anyhow::bail!("route add failed: {stderr}")
            }
        }
    }

    async fn add_dev_route(&self, ip: IpAddr, device: &str) -> Result<()> {
        tracing::info!(ip = %ip, device = device, "Adding route via device");

        let mut args = vec!["-n", "add"];
        if ip.is_ipv6() {
            args.push("-inet6");
        }
        let ip_str = ip.to_string();
        args.extend(["-host", &ip_str, "-interface", device]);

        let output = Command::new("/sbin/route")
            .args(&args)
            .output()
            .await?;

        if output.status.success() {
            tracing::debug!(ip = %ip, device = device, "Route added successfully");
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("File exists") {
                tracing::debug!(ip = %ip, "Route already exists");
                Ok(())
            } else {
                tracing::error!(ip = %ip, stderr = %stderr, "Failed to add route");
                anyhow::bail!("route add failed: {stderr}")
            }
        }
    }
}
