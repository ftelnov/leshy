mod config;
mod dns;
mod error;
mod reload;
mod routing;
mod service;
mod zones;

use clap::{Parser, Subcommand};
use config::Config;
use dns::{DnsHandler, DnsServer};
use reload::{get_new_zones, get_zones_to_cleanup, ConfigWatcher};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;
use zones::ZoneMatcher;

#[derive(Parser)]
#[command(name = "leshy", about = "DNS-driven split-tunnel router", version)]
struct Cli {
    /// Path to configuration file
    #[arg(global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Manage system service installation
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Install as a system service (systemd on Linux, launchd on macOS)
    Install {
        /// Path to configuration file for the service
        #[arg(long, default_value = service::default_config())]
        config: PathBuf,

        /// Service name (allows running multiple instances)
        #[arg(long, default_value = service::default_name())]
        name: String,
    },
    /// Remove the system service
    Uninstall {
        /// Service name to uninstall
        #[arg(long, default_value = service::default_name())]
        name: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Service { action }) => match action {
            ServiceAction::Install { config, name } => {
                service::install(Some(&name), Some(&config))?;
            }
            ServiceAction::Uninstall { name } => {
                service::uninstall(Some(&name))?;
            }
        },
        None => run_server(cli.config).await?,
    }

    Ok(())
}

async fn run_server(config_arg: Option<PathBuf>) -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config_path = if let Some(path) = config_arg {
        path
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

    // Apply static routes (and spawn retry loop for dev zones where VPN may not be up yet)
    {
        let handler_guard = handler.read().await;
        let failures = handler_guard.apply_static_routes().await;
        if failures > 0 && handler_guard.has_static_routes() {
            let handler_retry = handler.clone();
            tokio::spawn(async move {
                retry_static_routes(handler_retry).await;
            });
        }
    }

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
        let handler_for_reload = handler.clone();
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
                            let failures = handler_guard.apply_static_routes().await;
                            if failures > 0 && handler_guard.has_static_routes() {
                                let handler_retry = handler_for_reload.clone();
                                tokio::spawn(async move {
                                    retry_static_routes(handler_retry).await;
                                });
                            }
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

/// Retry applying static routes every 10 seconds until all succeed.
/// Handles the case where VPN device files don't exist yet at startup.
async fn retry_static_routes(handler: Arc<RwLock<DnsHandler>>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let handler_guard = handler.read().await;
        let failures = handler_guard.apply_static_routes().await;
        if failures == 0 {
            tracing::info!("All static routes applied successfully");
            break;
        }
        tracing::debug!(
            pending = failures,
            "Some static routes still pending, will retry"
        );
    }
}
