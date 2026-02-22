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
    async fn add_via_route(&self, ip: IpAddr, prefix_len: u8, gateway: &str) -> Result<()> {
        tracing::info!(ip = %ip, prefix_len = prefix_len, gateway = %gateway, "Adding route via gateway");

        let max_prefix = if ip.is_ipv6() { 128 } else { 32 };
        let is_host = prefix_len == max_prefix;

        let mut args = vec!["-n", "add"];
        if ip.is_ipv6() {
            args.push("-inet6");
        }
        let dest = if is_host {
            ip.to_string()
        } else {
            format!("{ip}/{prefix_len}")
        };
        if is_host {
            args.extend(["-host", &dest, gateway]);
        } else {
            args.extend(["-net", &dest, gateway]);
        }

        let output = Command::new("/sbin/route").args(&args).output().await?;

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

    async fn add_dev_route(&self, ip: IpAddr, prefix_len: u8, device: &str) -> Result<()> {
        tracing::info!(ip = %ip, prefix_len = prefix_len, device = device, "Adding route via device");

        let max_prefix = if ip.is_ipv6() { 128 } else { 32 };
        let is_host = prefix_len == max_prefix;

        let mut args = vec!["-n", "add"];
        if ip.is_ipv6() {
            args.push("-inet6");
        }
        let dest = if is_host {
            ip.to_string()
        } else {
            format!("{ip}/{prefix_len}")
        };
        if is_host {
            args.extend(["-host", &dest, "-interface", device]);
        } else {
            args.extend(["-net", &dest, "-interface", device]);
        }

        let output = Command::new("/sbin/route").args(&args).output().await?;

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

    async fn remove_route(&self, ip: IpAddr, prefix_len: u8) -> Result<()> {
        tracing::info!(ip = %ip, prefix_len = prefix_len, "Removing route");

        let max_prefix = if ip.is_ipv6() { 128 } else { 32 };
        let is_host = prefix_len == max_prefix;

        let mut args = vec!["-n", "delete"];
        if ip.is_ipv6() {
            args.push("-inet6");
        }
        let dest = if is_host {
            ip.to_string()
        } else {
            format!("{ip}/{prefix_len}")
        };
        if is_host {
            args.extend(["-host", &dest]);
        } else {
            args.extend(["-net", &dest]);
        }

        let output = Command::new("/sbin/route").args(&args).output().await?;

        if output.status.success() {
            tracing::debug!(ip = %ip, prefix_len = prefix_len, "Route removed successfully");
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not in table") {
                tracing::debug!(ip = %ip, "Route does not exist, nothing to remove");
                Ok(())
            } else {
                tracing::error!(ip = %ip, stderr = %stderr, "Failed to remove route");
                anyhow::bail!("route delete failed: {stderr}")
            }
        }
    }
}
