mod config;
mod dns;
mod error;
mod reload;
mod routing;
mod zones;

use config::Config;
use dns::{DnsHandler, DnsServer};
use reload::{get_new_zones, get_zones_to_cleanup, ConfigWatcher};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;
use zones::ZoneMatcher;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let config_path = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        // Try common locations
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let candidates = vec![
            PathBuf::from("leshy.toml"),  // Current directory
            PathBuf::from("config.toml"), // Current directory
            PathBuf::from(format!("{home}/.config/leshy/config.toml")),
            PathBuf::from("/etc/leshy/config.toml"),
        ];

        candidates
            .into_iter()
            .find(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("/etc/leshy/config.toml"))
    };

    tracing::info!(config_path = ?config_path, "Loading configuration");

    // Load configuration (includes config.d directory if present)
    let config = Config::from_file_with_includes(&config_path)?;
    let auto_reload = config.server.auto_reload;

    tracing::info!(
        listen = %config.server.listen_address,
        zones = config.zones.len(),
        auto_reload = auto_reload,
        "Configuration loaded"
    );

    // Create zone matcher
    let matcher = ZoneMatcher::new(config.zones.clone())?;

    // Create DNS handler (wrapped in Arc for reload)
    let handler = Arc::new(RwLock::new(DnsHandler::new(config.clone(), matcher)?));

    // Create and start DNS server
    let server = DnsServer::new(config.server.listen_address, handler.clone()).await?;

    tracing::info!("Leshy DNS server started");

    // Spawn config watcher if auto_reload is enabled
    if auto_reload {
        let handler_clone = handler.clone();
        let config_dir = config.server.config_dir.as_ref().map(PathBuf::from);
        let (watcher, mut reload_rx) = ConfigWatcher::new(config_path.clone(), config_dir);

        // Spawn watcher task
        tokio::spawn(async move {
            if let Err(e) = watcher.watch().await {
                tracing::error!("Config watcher error: {}", e);
            }
        });

        // Spawn reload handler task
        tokio::spawn(async move {
            while let Some(new_config) = reload_rx.recv().await {
                tracing::info!("Applying new configuration");

                // Get current handler
                let mut handler_guard = handler_clone.write().await;
                let old_config = handler_guard.config().clone();

                // Determine zones to cleanup and new zones
                let zones_to_cleanup = get_zones_to_cleanup(&old_config.zones, &new_config.zones);
                let new_zones = get_new_zones(&old_config.zones, &new_config.zones);

                // Cleanup routes for removed zones
                for zone_name in zones_to_cleanup {
                    tracing::info!(zone = zone_name, "Removing zone and cleaning up routes");
                    if let Err(e) = handler_guard.cleanup_zone(&zone_name).await {
                        tracing::error!(zone = zone_name, error = %e, "Failed to cleanup zone");
                    }
                }

                // Create new matcher with updated zones
                match ZoneMatcher::new(new_config.zones.clone()) {
                    Ok(new_matcher) => {
                        // Update handler with new config and matcher
                        if let Err(e) = handler_guard
                            .update_config(new_config.clone(), new_matcher)
                            .await
                        {
                            tracing::error!(error = %e, "Failed to update handler config");
                        } else {
                            tracing::info!(
                                zones_added = new_zones.len(),
                                total_zones = new_config.zones.len(),
                                "Configuration applied successfully"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to create zone matcher, keeping old config");
                    }
                }
            }
        });
    }

    // Run server
    server.run().await?;

    Ok(())
}
