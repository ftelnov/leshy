use crate::config::{Config, ZoneConfig};
use anyhow::Result;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Watches config file for changes and sends reload signals
pub struct ConfigWatcher {
    config_path: PathBuf,
    config_dir: Option<PathBuf>,
    reload_tx: mpsc::UnboundedSender<Config>,
}

impl ConfigWatcher {
    pub fn new(
        config_path: PathBuf,
        config_dir: Option<PathBuf>,
    ) -> (Self, mpsc::UnboundedReceiver<Config>) {
        let (reload_tx, reload_rx) = mpsc::unbounded_channel();
        (
            Self {
                config_path,
                config_dir,
                reload_tx,
            },
            reload_rx,
        )
    }

    /// Start watching the config file and config.d directory for changes
    pub async fn watch(self) -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel::<notify::Result<Event>>();
        let config_path = self.config_path.clone();
        let reload_tx = self.reload_tx.clone();

        // Spawn file watcher in blocking task
        let watch_path = config_path.clone();
        let explicit_config_dir = self.config_dir.clone();
        tokio::task::spawn_blocking(move || {
            let mut watcher = RecommendedWatcher::new(
                move |res: notify::Result<Event>| {
                    let _ = tx.send(res);
                },
                notify::Config::default(),
            )
            .expect("Failed to create watcher");

            // Watch main config file
            if let Err(e) = watcher.watch(&watch_path, RecursiveMode::NonRecursive) {
                error!("Failed to watch config file: {}", e);
                return;
            }

            info!("Watching config file for changes: {}", watch_path.display());

            // Watch config.d directory if it exists
            // Try explicit config_dir first, then look next to config file
            let candidates: Vec<std::path::PathBuf> = vec![
                explicit_config_dir.clone(),
                watch_path.parent().map(|p| p.join("config.d")),
            ]
            .into_iter()
            .flatten()
            .collect();

            for config_dir in candidates {
                if config_dir.exists() && config_dir.is_dir() {
                    if let Err(e) = watcher.watch(&config_dir, RecursiveMode::Recursive) {
                        warn!("Failed to watch config.d directory: {}", e);
                    } else {
                        info!("Watching config.d directory: {}", config_dir.display());
                    }
                    break;
                }
            }

            // Keep watcher alive
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        });

        // Process file change events
        while let Some(event_result) = rx.recv().await {
            match event_result {
                Ok(event) => {
                    if matches!(
                        event.kind,
                        notify::EventKind::Modify(_)
                            | notify::EventKind::Create(_)
                            | notify::EventKind::Remove(_)
                    ) {
                        info!("Config changed, reloading...");
                        match Config::from_file_with_includes(&config_path) {
                            Ok(new_config) => {
                                info!("Config reloaded successfully");
                                if let Err(e) = reload_tx.send(new_config) {
                                    error!("Failed to send reload signal: {}", e);
                                    break;
                                }
                            }
                            Err(e) => {
                                warn!("Failed to reload config, keeping old config: {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Watch error: {}", e);
                }
            }
        }

        Ok(())
    }
}

/// Compares two zone configurations and returns zones that need cleanup
pub fn get_zones_to_cleanup(old_zones: &[ZoneConfig], new_zones: &[ZoneConfig]) -> Vec<String> {
    let old_zone_names: HashSet<String> = old_zones.iter().map(|z| z.name.clone()).collect();
    let new_zone_names: HashSet<String> = new_zones.iter().map(|z| z.name.clone()).collect();

    // Zones that are in old but not in new need cleanup
    old_zone_names
        .difference(&new_zone_names)
        .cloned()
        .collect()
}

/// Compares two zone configurations and returns new zones
pub fn get_new_zones(old_zones: &[ZoneConfig], new_zones: &[ZoneConfig]) -> Vec<ZoneConfig> {
    let old_zone_names: HashSet<String> = old_zones.iter().map(|z| z.name.clone()).collect();

    new_zones
        .iter()
        .filter(|z| !old_zone_names.contains(&z.name))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{RouteType, ZoneConfig};

    fn test_zone(name: &str, route_type: RouteType, route_target: &str) -> ZoneConfig {
        ZoneConfig {
            name: name.to_string(),
            dns_servers: vec![],
            route_type,
            route_target: route_target.to_string(),
            domains: vec![],
            patterns: vec![],
            static_routes: vec![],
            dns_protocol: Default::default(),
            cache_min_ttl: None,
            cache_max_ttl: None,
            cache_negative_ttl: None,
        }
    }

    #[test]
    fn test_get_zones_to_cleanup() {
        let old_zones = vec![
            test_zone("zone1", RouteType::Via, "192.168.1.1"),
            test_zone("zone2", RouteType::Via, "192.168.1.1"),
        ];

        let new_zones = vec![test_zone("zone2", RouteType::Via, "192.168.1.1")];

        let to_cleanup = get_zones_to_cleanup(&old_zones, &new_zones);
        assert_eq!(to_cleanup.len(), 1);
        assert!(to_cleanup.contains(&"zone1".to_string()));
    }

    #[test]
    fn test_get_new_zones() {
        let old_zones = vec![test_zone("zone1", RouteType::Via, "192.168.1.1")];

        let new_zones = vec![
            test_zone("zone1", RouteType::Via, "192.168.1.1"),
            test_zone("zone2", RouteType::Dev, "/tmp/test.dev"),
        ];

        let new = get_new_zones(&old_zones, &new_zones);
        assert_eq!(new.len(), 1);
        assert_eq!(new[0].name, "zone2");
    }
}
