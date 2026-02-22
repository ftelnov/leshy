use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

fn plist_label(name: &str) -> String {
    format!("com.{name}.server")
}

fn plist_path(name: &str) -> PathBuf {
    PathBuf::from(format!(
        "/Library/LaunchDaemons/{}.plist",
        plist_label(name)
    ))
}

fn generate_plist(name: &str, binary: &Path, config: &Path) -> String {
    let label = plist_label(name);
    let binary = binary.display();
    let config = config.display();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>{config}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/var/log/{name}.log</string>
    <key>StandardErrorPath</key>
    <string>/var/log/{name}.err</string>
</dict>
</plist>
"#
    )
}

pub fn install(name: &str, binary: &Path, config: &Path) -> Result<()> {
    let path = plist_path(name);
    let plist = generate_plist(name, binary, config);

    std::fs::write(&path, &plist)
        .with_context(|| format!("failed to write plist to {}", path.display()))?;
    println!("Wrote {}", path.display());

    let status = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&path)
        .status()
        .context("failed to run launchctl load")?;
    if !status.success() {
        anyhow::bail!("launchctl load failed");
    }

    println!(
        "Service {} loaded. It will start automatically.",
        plist_label(name)
    );
    Ok(())
}

pub fn uninstall(name: &str) -> Result<()> {
    let path = plist_path(name);

    if path.exists() {
        let _ = Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&path)
            .status();

        std::fs::remove_file(&path)
            .with_context(|| format!("failed to remove {}", path.display()))?;
        println!("Removed {}", path.display());
    } else {
        println!("Plist {} does not exist, nothing to remove", path.display());
    }

    println!("Service {} uninstalled", plist_label(name));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn plist_contains_binary_and_config() {
        let plist = generate_plist(
            "leshy",
            Path::new("/usr/local/bin/leshy"),
            Path::new("/etc/leshy/config.toml"),
        );
        assert!(plist.contains("<string>/usr/local/bin/leshy</string>"));
        assert!(plist.contains("<string>/etc/leshy/config.toml</string>"));
        assert!(plist.contains("com.leshy.server"));
    }

    #[test]
    fn custom_name_in_plist_label() {
        let plist = generate_plist(
            "leshy-corp",
            Path::new("/usr/local/bin/leshy"),
            Path::new("/etc/leshy/corp.toml"),
        );
        assert!(plist.contains("com.leshy-corp.server"));
    }
}
