use crate::config::RouteType;
use std::collections::HashMap;
use std::net::Ipv4Addr;

/// Describes a kernel route action the caller must execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteAction {
    Add {
        network: Ipv4Addr,
        prefix_len: u8,
        route_type: RouteType,
        route_target: String,
    },
    Remove {
        network: Ipv4Addr,
        prefix_len: u8,
    },
}

#[derive(Debug, Clone)]
struct RouteOwner {
    zone_name: String,
    route_type: RouteType,
    route_target: String,
}

/// Aggregates individual /32 host routes into wider CIDR prefixes to reduce
/// the size of the kernel routing table.
///
/// When aggregation is enabled (prefix < 32), adding an IP installs a wider
/// prefix (e.g. /22) covering that IP. Future IPs in the same range and zone
/// are automatic no-ops. If an IP from a *different* zone falls into an
/// existing aggregate, the aggregate is split into non-conflicting sub-prefixes.
pub struct RouteAggregator {
    /// Installed kernel routes: (network_addr_as_u32, prefix_len) -> owner
    installed: HashMap<(u32, u8), RouteOwner>,
    /// Ground truth: individual IP -> zone name (for conflict detection)
    known_ips: HashMap<Ipv4Addr, String>,
    /// Target aggregation prefix length (e.g. 22 for /22). 32 = disabled.
    prefix_len: u8,
}

impl RouteAggregator {
    pub fn new(prefix_len: Option<u8>) -> Self {
        Self {
            installed: HashMap::new(),
            known_ips: HashMap::new(),
            prefix_len: prefix_len.unwrap_or(32),
        }
    }

    /// Main entry point: process an IP and return kernel route actions.
    pub fn process_ip(
        &mut self,
        ip: Ipv4Addr,
        zone_name: &str,
        route_type: RouteType,
        route_target: &str,
    ) -> Vec<RouteAction> {
        // Record this IP's zone ownership
        self.known_ips.insert(ip, zone_name.to_string());

        // Disabled (prefix_len == 32): always install /32
        if self.prefix_len >= 32 {
            let key = (u32::from(ip), 32);
            if self.installed.contains_key(&key) {
                return vec![];
            }
            self.installed.insert(
                key,
                RouteOwner {
                    zone_name: zone_name.to_string(),
                    route_type,
                    route_target: route_target.to_string(),
                },
            );
            return vec![RouteAction::Add {
                network: ip,
                prefix_len: 32,
                route_type,
                route_target: route_target.to_string(),
            }];
        }

        // Check if IP is already covered by an installed aggregate
        if let Some((existing_key, existing_owner)) = self.find_covering_route(ip) {
            if existing_owner.zone_name == zone_name {
                // Same zone — already covered, no-op
                return vec![];
            }

            // Different zone — must split the existing aggregate
            let old_net = existing_key.0;
            let old_prefix = existing_key.1;
            let old_owner = existing_owner.clone();
            self.installed.remove(&(old_net, old_prefix));

            let mut actions = vec![RouteAction::Remove {
                network: Ipv4Addr::from(old_net),
                prefix_len: old_prefix,
            }];

            // Split: repeatedly halve, adding the half that does NOT contain
            // the conflicting IP, until we reach /32
            let mut cur_net = old_net;
            let mut cur_prefix = old_prefix;

            while cur_prefix < 32 {
                let child_prefix = cur_prefix + 1;
                let (left, right) = split_network(cur_net, cur_prefix);

                let ip_u32 = u32::from(ip);
                let (contains_ip, sibling) = if ip_in_network(ip_u32, left, child_prefix) {
                    (left, right)
                } else {
                    (right, left)
                };

                // Install sibling for original zone
                self.installed.insert(
                    (sibling, child_prefix),
                    RouteOwner {
                        zone_name: old_owner.zone_name.clone(),
                        route_type: old_owner.route_type,
                        route_target: old_owner.route_target.clone(),
                    },
                );
                actions.push(RouteAction::Add {
                    network: Ipv4Addr::from(sibling),
                    prefix_len: child_prefix,
                    route_type: old_owner.route_type,
                    route_target: old_owner.route_target.clone(),
                });

                cur_net = contains_ip;
                cur_prefix = child_prefix;
            }

            // Install /32 for the new (conflicting) IP
            self.installed.insert(
                (u32::from(ip), 32),
                RouteOwner {
                    zone_name: zone_name.to_string(),
                    route_type,
                    route_target: route_target.to_string(),
                },
            );
            actions.push(RouteAction::Add {
                network: ip,
                prefix_len: 32,
                route_type,
                route_target: route_target.to_string(),
            });

            return actions;
        }

        // Not covered — create a new aggregate
        let agg_net = network_address(u32::from(ip), self.prefix_len);

        // Check if any known IPs from OTHER zones fall within this aggregate
        let conflicts: Vec<(Ipv4Addr, String)> = self
            .known_ips
            .iter()
            .filter(|(known_ip, known_zone)| {
                *known_zone != zone_name
                    && ip_in_network(u32::from(**known_ip), agg_net, self.prefix_len)
            })
            .map(|(ip, zone)| (*ip, zone.clone()))
            .collect();

        if conflicts.is_empty() {
            // No conflicts — install the full aggregate
            self.installed.insert(
                (agg_net, self.prefix_len),
                RouteOwner {
                    zone_name: zone_name.to_string(),
                    route_type,
                    route_target: route_target.to_string(),
                },
            );
            return vec![RouteAction::Add {
                network: Ipv4Addr::from(agg_net),
                prefix_len: self.prefix_len,
                route_type,
                route_target: route_target.to_string(),
            }];
        }

        // Conflicts exist — install the aggregate then carve out each conflict
        self.installed.insert(
            (agg_net, self.prefix_len),
            RouteOwner {
                zone_name: zone_name.to_string(),
                route_type,
                route_target: route_target.to_string(),
            },
        );
        let mut actions = vec![RouteAction::Add {
            network: Ipv4Addr::from(agg_net),
            prefix_len: self.prefix_len,
            route_type,
            route_target: route_target.to_string(),
        }];

        // For each conflicting IP, split around it
        for (conflict_ip, _conflict_zone) in &conflicts {
            // Find which installed aggregate currently covers this conflict
            if let Some((cov_key, cov_owner)) = self.find_covering_route(*conflict_ip) {
                if cov_owner.zone_name == zone_name {
                    // The aggregate we just installed covers this conflict — split it
                    let cov_net = cov_key.0;
                    let cov_prefix = cov_key.1;
                    let cov_owner = cov_owner.clone();
                    self.installed.remove(&(cov_net, cov_prefix));

                    actions.push(RouteAction::Remove {
                        network: Ipv4Addr::from(cov_net),
                        prefix_len: cov_prefix,
                    });

                    let mut cur_net = cov_net;
                    let mut cur_prefix = cov_prefix;
                    let conflict_u32 = u32::from(*conflict_ip);

                    while cur_prefix < 32 {
                        let child_prefix = cur_prefix + 1;
                        let (left, right) = split_network(cur_net, cur_prefix);

                        let (contains_conflict, sibling) =
                            if ip_in_network(conflict_u32, left, child_prefix) {
                                (left, right)
                            } else {
                                (right, left)
                            };

                        self.installed.insert(
                            (sibling, child_prefix),
                            RouteOwner {
                                zone_name: cov_owner.zone_name.clone(),
                                route_type: cov_owner.route_type,
                                route_target: cov_owner.route_target.clone(),
                            },
                        );
                        actions.push(RouteAction::Add {
                            network: Ipv4Addr::from(sibling),
                            prefix_len: child_prefix,
                            route_type: cov_owner.route_type,
                            route_target: cov_owner.route_target.clone(),
                        });

                        cur_net = contains_conflict;
                        cur_prefix = child_prefix;
                    }

                    // The /32 slot for the conflict IP is now empty — don't install
                    // anything there (it belongs to another zone and will be
                    // installed when that zone's aggregator path runs for it,
                    // or it was already installed previously).
                }
            }
        }

        actions
    }

    /// Register a static route's IPs so aggregates don't overlap with them.
    /// Does NOT return actions (static routes are installed directly).
    pub fn register_static_ip(&mut self, ip: Ipv4Addr, zone_name: &str) {
        self.known_ips.insert(ip, zone_name.to_string());
    }

    /// Remove all tracking for a zone.
    pub fn cleanup_zone(&mut self, zone_name: &str) {
        self.installed
            .retain(|_, owner| owner.zone_name != zone_name);
        self.known_ips.retain(|_, zone| zone != zone_name);
    }

    /// Find an installed route that covers the given IP.
    /// Returns the key and a reference to the owner.
    fn find_covering_route(&self, ip: Ipv4Addr) -> Option<((u32, u8), &RouteOwner)> {
        let ip_u32 = u32::from(ip);
        // Check from most-specific to least-specific
        for prefix in (0..=32).rev() {
            let net = network_address(ip_u32, prefix);
            if let Some(owner) = self.installed.get(&(net, prefix)) {
                return Some(((net, prefix), owner));
            }
        }
        None
    }
}

/// Compute the network address for an IP at a given prefix length.
fn network_address(ip: u32, prefix_len: u8) -> u32 {
    if prefix_len == 0 {
        0
    } else {
        ip & !((1u32 << (32 - prefix_len)) - 1)
    }
}

/// Split a network into its two child halves (prefix_len + 1).
fn split_network(net: u32, prefix_len: u8) -> (u32, u32) {
    let child_prefix = prefix_len + 1;
    let left = net;
    let right = net | (1u32 << (32 - child_prefix));
    (left, right)
}

/// Check if an IP (as u32) is within a network/prefix.
fn ip_in_network(ip: u32, network: u32, prefix_len: u8) -> bool {
    network_address(ip, prefix_len) == network
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_aggregation() {
        let mut agg = RouteAggregator::new(Some(24));
        let actions = agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 5),
            "zone1",
            RouteType::Via,
            "192.168.1.1",
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            RouteAction::Add {
                network: Ipv4Addr::new(10, 0, 0, 0),
                prefix_len: 24,
                route_type: RouteType::Via,
                route_target: "192.168.1.1".to_string(),
            }
        );
    }

    #[test]
    fn same_zone_noop() {
        let mut agg = RouteAggregator::new(Some(24));
        agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 5),
            "zone1",
            RouteType::Via,
            "192.168.1.1",
        );

        // Second IP in same /24, same zone — no new actions
        let actions = agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 100),
            "zone1",
            RouteType::Via,
            "192.168.1.1",
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn cross_zone_conflict_splits_aggregate() {
        let mut agg = RouteAggregator::new(Some(24));
        agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 5),
            "zone1",
            RouteType::Via,
            "192.168.1.1",
        );

        // Different zone, same /24 — must split
        let actions = agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 200),
            "zone2",
            RouteType::Via,
            "192.168.2.1",
        );

        // Should have: 1 Remove + 8 sibling Adds (24->32 = 8 splits) + 1 /32 Add = 10 actions
        let removes: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, RouteAction::Remove { .. }))
            .collect();
        let adds: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, RouteAction::Add { .. }))
            .collect();

        assert_eq!(removes.len(), 1);
        assert_eq!(
            removes[0],
            &RouteAction::Remove {
                network: Ipv4Addr::new(10, 0, 0, 0),
                prefix_len: 24,
            }
        );

        // 8 siblings for zone1 + 1 /32 for zone2 = 9 adds
        assert_eq!(adds.len(), 9);

        // The last add should be the /32 for the conflicting IP
        assert_eq!(
            *adds.last().unwrap(),
            &RouteAction::Add {
                network: Ipv4Addr::new(10, 0, 0, 200),
                prefix_len: 32,
                route_type: RouteType::Via,
                route_target: "192.168.2.1".to_string(),
            }
        );
    }

    #[test]
    fn new_aggregate_with_preexisting_conflicts() {
        let mut agg = RouteAggregator::new(Some(24));

        // First, add an IP in zone2 at 10.0.0.100
        agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 100),
            "zone2",
            RouteType::Via,
            "192.168.2.1",
        );

        // Now add an IP in zone1 at 10.0.0.5 — same /24, but zone1 wants the aggregate
        // The aggregate for zone1 must carve out 10.0.0.100 which belongs to zone2
        let actions = agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 5),
            "zone1",
            RouteType::Via,
            "192.168.1.1",
        );

        // Should install the /24 aggregate, then immediately split around
        // the conflicting 10.0.0.100
        let adds: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, RouteAction::Add { .. }))
            .collect();
        let removes: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, RouteAction::Remove { .. }))
            .collect();

        // At least one add and one remove (the original /24 gets removed during carve-out)
        assert!(!adds.is_empty());
        assert!(!removes.is_empty());

        // The /32 for 10.0.0.100 should NOT be in the adds (it belongs to zone2,
        // already installed). Verify no /32 for 10.0.0.100 with zone1's target.
        let conflict_adds: Vec<_> = adds
            .iter()
            .filter(|a| {
                matches!(a, RouteAction::Add { network, prefix_len: 32, .. } if *network == Ipv4Addr::new(10, 0, 0, 100))
            })
            .collect();
        assert!(conflict_adds.is_empty());
    }

    #[test]
    fn disabled_always_returns_32() {
        // prefix_len = 32 means disabled
        let mut agg = RouteAggregator::new(Some(32));
        let actions = agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 5),
            "zone1",
            RouteType::Via,
            "192.168.1.1",
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            RouteAction::Add {
                network: Ipv4Addr::new(10, 0, 0, 5),
                prefix_len: 32,
                route_type: RouteType::Via,
                route_target: "192.168.1.1".to_string(),
            }
        );

        // Same IP again — no-op
        let actions2 = agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 5),
            "zone1",
            RouteType::Via,
            "192.168.1.1",
        );
        assert!(actions2.is_empty());
    }

    #[test]
    fn disabled_none_always_returns_32() {
        // None means disabled
        let mut agg = RouteAggregator::new(None);
        let actions = agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 5),
            "zone1",
            RouteType::Via,
            "192.168.1.1",
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0],
            RouteAction::Add {
                network: Ipv4Addr::new(10, 0, 0, 5),
                prefix_len: 32,
                route_type: RouteType::Via,
                route_target: "192.168.1.1".to_string(),
            }
        );
    }

    #[test]
    fn cleanup_zone_removes_tracking() {
        let mut agg = RouteAggregator::new(Some(24));
        agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 5),
            "zone1",
            RouteType::Via,
            "192.168.1.1",
        );
        agg.process_ip(
            Ipv4Addr::new(10, 1, 0, 5),
            "zone2",
            RouteType::Via,
            "192.168.2.1",
        );

        agg.cleanup_zone("zone1");

        // zone1's aggregate should be gone from installed
        assert!(!agg.installed.values().any(|o| o.zone_name == "zone1"));
        // zone1's known IPs should be gone
        assert!(!agg.known_ips.values().any(|z| z == "zone1"));
        // zone2 should still be present
        assert!(agg.installed.values().any(|o| o.zone_name == "zone2"));
    }

    #[test]
    fn register_static_ip_prevents_overlap() {
        let mut agg = RouteAggregator::new(Some(24));

        // Register a static IP for zone2 in the 10.0.0.0/24 range
        agg.register_static_ip(Ipv4Addr::new(10, 0, 0, 50), "zone2");

        // Now zone1 wants to aggregate in that range — should carve out 10.0.0.50
        let actions = agg.process_ip(
            Ipv4Addr::new(10, 0, 0, 5),
            "zone1",
            RouteType::Via,
            "192.168.1.1",
        );

        // Should have carve-out: initial add + remove + sibling adds
        let removes: Vec<_> = actions
            .iter()
            .filter(|a| matches!(a, RouteAction::Remove { .. }))
            .collect();
        assert!(!removes.is_empty());
    }

    #[test]
    fn network_address_computation() {
        assert_eq!(
            network_address(u32::from(Ipv4Addr::new(10, 0, 0, 5)), 24),
            u32::from(Ipv4Addr::new(10, 0, 0, 0))
        );
        assert_eq!(
            network_address(u32::from(Ipv4Addr::new(10, 0, 0, 255)), 24),
            u32::from(Ipv4Addr::new(10, 0, 0, 0))
        );
        assert_eq!(
            network_address(u32::from(Ipv4Addr::new(104, 16, 132, 229)), 22),
            u32::from(Ipv4Addr::new(104, 16, 132, 0))
        );
        assert_eq!(
            network_address(u32::from(Ipv4Addr::new(192, 168, 1, 100)), 32),
            u32::from(Ipv4Addr::new(192, 168, 1, 100))
        );
    }

    #[test]
    fn split_network_correctness() {
        let net = u32::from(Ipv4Addr::new(10, 0, 0, 0));
        let (left, right) = split_network(net, 24);
        assert_eq!(left, u32::from(Ipv4Addr::new(10, 0, 0, 0)));
        assert_eq!(right, u32::from(Ipv4Addr::new(10, 0, 0, 128)));
    }
}
