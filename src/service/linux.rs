use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

fn unit_path(name: &str) -> PathBuf {
    PathBuf::from(format!("/etc/systemd/system/{name}.service"))
}

fn generate_unit(name: &str, binary: &Path, config: &Path) -> String {
    let binary = binary.display();
    let config = config.display();
    format!(
        "\
[Unit]
Description={name} DNS-driven split-tunnel router
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={binary} {config}
Restart=on-failure
RestartSec=5
AmbientCapabilities=CAP_NET_ADMIN CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
"
    )
}

pub fn install(name: &str, binary: &Path, config: &Path) -> Result<()> {
    let path = unit_path(name);
    let unit = generate_unit(name, binary, config);

    std::fs::write(&path, &unit)
        .with_context(|| format!("failed to write unit file to {}", path.display()))?;
    println!("Wrote {}", path.display());

    let status = Command::new("systemctl")
        .args(["daemon-reload"])
        .status()
        .context("failed to run systemctl daemon-reload")?;
    if !status.success() {
        anyhow::bail!("systemctl daemon-reload failed");
    }

    let status = Command::new("systemctl")
        .args(["enable", name])
        .status()
        .context("failed to run systemctl enable")?;
    if !status.success() {
        anyhow::bail!("systemctl enable {name} failed");
    }

    println!("Service {name} enabled. Start it with: sudo systemctl start {name}");
    Ok(())
}

pub fn uninstall(name: &str) -> Result<()> {
    let path = unit_path(name);

    // Stop and disable (best-effort)
    let _ = Command::new("systemctl").args(["stop", name]).status();
    let _ = Command::new("systemctl").args(["disable", name]).status();

    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
        println!("Removed {}", path.display());
    } else {
        println!(
            "Unit file {} does not exist, nothing to remove",
            path.display()
        );
    }

    let _ = Command::new("systemctl").args(["daemon-reload"]).status();

    println!("Service {name} uninstalled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn unit_file_contains_capabilities() {
        let unit = generate_unit(
            "leshy",
            Path::new("/usr/local/bin/leshy"),
            Path::new("/etc/leshy/config.toml"),
        );
        assert!(unit.contains("CAP_NET_ADMIN"));
        assert!(unit.contains("CAP_NET_BIND_SERVICE"));
        assert!(unit.contains("/usr/local/bin/leshy /etc/leshy/config.toml"));
    }

    #[test]
    fn custom_name_in_unit_description() {
        let unit = generate_unit(
            "leshy-corp",
            Path::new("/usr/local/bin/leshy"),
            Path::new("/etc/leshy/corp.toml"),
        );
        assert!(unit.contains("Description=leshy-corp"));
    }
}
