#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use anyhow::Result;
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG: &str = "/etc/leshy/config.toml";
const DEFAULT_NAME: &str = "leshy";
const FALLBACK_BINARY: &str = "/usr/local/bin/leshy";

fn detect_binary() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .unwrap_or_else(|| PathBuf::from(FALLBACK_BINARY))
}

pub fn default_config() -> &'static str {
    DEFAULT_CONFIG
}

pub fn default_name() -> &'static str {
    DEFAULT_NAME
}

pub fn install(name: Option<&str>, config: Option<&Path>) -> Result<()> {
    let name = name.unwrap_or(DEFAULT_NAME);
    let config = config.unwrap_or_else(|| Path::new(DEFAULT_CONFIG));
    let binary = detect_binary();

    println!(
        "Installing service '{name}' (binary: {}, config: {})",
        binary.display(),
        config.display()
    );

    #[cfg(target_os = "linux")]
    linux::install(name, &binary, config)?;

    #[cfg(target_os = "macos")]
    macos::install(name, &binary, config)?;

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("service install is not supported on this platform");

    Ok(())
}

pub fn uninstall(name: Option<&str>) -> Result<()> {
    let name = name.unwrap_or(DEFAULT_NAME);

    println!("Uninstalling service '{name}'");

    #[cfg(target_os = "linux")]
    linux::uninstall(name)?;

    #[cfg(target_os = "macos")]
    macos::uninstall(name)?;

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("service uninstall is not supported on this platform");

    Ok(())
}
