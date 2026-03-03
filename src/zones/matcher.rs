use crate::config::{ZoneConfig, ZoneMode};
use regex::RegexSet;
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

/// An IPv4 CIDR range used for per-zone IP exclusion checks.
#[derive(Debug, Clone)]
struct CidrRange {
    network: u32,
    prefix_len: u8,
}

impl CidrRange {
    fn contains_v4(&self, ip: Ipv4Addr) -> bool {
        if self.prefix_len == 0 {
            return true;
        }
        let mask = !((1u32 << (32 - self.prefix_len)) - 1);
        (u32::from(ip) & mask) == self.network
    }
}

/// Result of a zone match. Wraps the zone config and any per-zone exclusion CIDRs.
#[derive(Debug, Clone)]
pub struct MatchedZone {
    pub config: Arc<ZoneConfig>,
    excluded_cidrs: Vec<CidrRange>,
}

impl MatchedZone {
    /// Check if an IP falls within this zone's excluded CIDR ranges.
    /// Always false for inclusive zones (they have no excluded CIDRs).
    /// For exclusive zones, returns true if IPv4 matches any excluded range.
    pub fn is_excluded(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => self.excluded_cidrs.iter().any(|r| r.contains_v4(v4)),
            IpAddr::V6(_) => false,
        }
    }
}

/// Matches only listed domains/patterns. Routes all resolved IPs.
#[derive(Debug)]
struct InclusiveZone {
    config: Arc<ZoneConfig>,
    domain_set: HashSet<String>,
    pattern_set: RegexSet,
}

/// Matches everything EXCEPT listed domains/patterns.
/// Resolved IPs falling within `excluded_cidrs` are skipped.
#[derive(Debug)]
struct ExclusiveZone {
    config: Arc<ZoneConfig>,
    excluded_domains: HashSet<String>,
    excluded_patterns: RegexSet,
    excluded_cidrs: Vec<CidrRange>,
}

/// A zone with type-level distinction between inclusive and exclusive matching.
#[derive(Debug)]
enum Zone {
    Inclusive(InclusiveZone),
    Exclusive(ExclusiveZone),
}

#[derive(Debug)]
pub struct ZoneMatcher {
    zones: Vec<Zone>,
}

impl ZoneMatcher {
    pub fn new(zones: Vec<ZoneConfig>) -> anyhow::Result<Self> {
        let mut built = Vec::with_capacity(zones.len());

        for zone_cfg in zones {
            let domain_set: HashSet<String> =
                zone_cfg.domains.iter().map(|d| d.to_lowercase()).collect();

            let pattern_set = RegexSet::new(&zone_cfg.patterns).map_err(|e| {
                anyhow::anyhow!("Zone '{}': invalid regex pattern: {}", zone_cfg.name, e)
            })?;

            let config = Arc::new(zone_cfg);

            let zone = match config.mode {
                ZoneMode::Inclusive => Zone::Inclusive(InclusiveZone {
                    config,
                    domain_set,
                    pattern_set,
                }),
                ZoneMode::Exclusive => {
                    let excluded_cidrs = config
                        .static_routes
                        .iter()
                        .filter_map(|cidr| {
                            parse_cidr_range(cidr)
                                .map_err(|e| {
                                    tracing::warn!(
                                        cidr = cidr,
                                        zone = config.name,
                                        error = %e,
                                        "Failed to parse CIDR in exclusive zone, skipping"
                                    );
                                    e
                                })
                                .ok()
                        })
                        .collect();

                    Zone::Exclusive(ExclusiveZone {
                        config,
                        excluded_domains: domain_set,
                        excluded_patterns: pattern_set,
                        excluded_cidrs,
                    })
                }
            };

            built.push(zone);
        }

        Ok(Self { zones: built })
    }

    /// Find the first zone that matches the given query name.
    /// Returns a `MatchedZone` that includes per-zone exclusion CIDRs.
    pub fn find_zone(&self, qname: &str) -> Option<MatchedZone> {
        let qname = qname.trim_end_matches('.');

        for zone in &self.zones {
            match zone {
                Zone::Inclusive(z) => {
                    if matches_entries(&z.domain_set, &z.pattern_set, qname, &z.config.name) {
                        return Some(MatchedZone {
                            config: Arc::clone(&z.config),
                            excluded_cidrs: Vec::new(),
                        });
                    }
                }
                Zone::Exclusive(z) => {
                    let is_excluded = matches_entries(
                        &z.excluded_domains,
                        &z.excluded_patterns,
                        qname,
                        &z.config.name,
                    );
                    if !is_excluded {
                        tracing::debug!(
                            zone = z.config.name,
                            qname = qname,
                            "Exclusive zone match (not excluded)"
                        );
                        return Some(MatchedZone {
                            config: Arc::clone(&z.config),
                            excluded_cidrs: z.excluded_cidrs.clone(),
                        });
                    }
                    tracing::debug!(
                        zone = z.config.name,
                        qname = qname,
                        "Excluded from exclusive zone"
                    );
                }
            }
        }

        tracing::debug!(qname = qname, "No zone match, using default");
        None
    }
}

/// Check whether a domain matches any entry in the domain set or pattern set.
fn matches_entries(
    domain_set: &HashSet<String>,
    pattern_set: &RegexSet,
    qname: &str,
    zone_name: &str,
) -> bool {
    // Walk suffix labels for domain match
    let lower = qname.to_lowercase();
    let mut remaining = lower.as_str();
    loop {
        if domain_set.contains(remaining) {
            tracing::debug!(
                zone = zone_name,
                domain = remaining,
                qname = qname,
                "Domain match"
            );
            return true;
        }
        match remaining.find('.') {
            Some(pos) => remaining = &remaining[pos + 1..],
            None => break,
        }
    }

    // Pattern match (single RegexSet call)
    if pattern_set.is_match(qname) {
        tracing::debug!(zone = zone_name, qname = qname, "Pattern match");
        return true;
    }

    false
}

/// Parse a CIDR string like "10.0.0.0/8" into a CidrRange.
/// Only supports IPv4. Returns an error for IPv6 or invalid input.
fn parse_cidr_range(cidr: &str) -> anyhow::Result<CidrRange> {
    let (ip_str, prefix_len) = if let Some((ip, prefix)) = cidr.split_once('/') {
        let prefix_len: u8 = prefix
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid prefix length: {prefix}"))?;
        if prefix_len > 32 {
            anyhow::bail!("prefix length {prefix_len} exceeds 32");
        }
        (ip, prefix_len)
    } else {
        (cidr, 32u8)
    };

    let ip: Ipv4Addr = ip_str
        .parse()
        .map_err(|_| anyhow::anyhow!("not a valid IPv4 address: {ip_str}"))?;

    let mask = if prefix_len == 0 {
        0u32
    } else {
        !((1u32 << (32 - prefix_len)) - 1)
    };

    Ok(CidrRange {
        network: u32::from(ip) & mask,
        prefix_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_zone(name: &str, domains: Vec<&str>, patterns: Vec<&str>) -> ZoneConfig {
        ZoneConfig {
            name: name.to_string(),
            mode: Default::default(),
            dns_servers: vec![],
            route_type: crate::config::RouteType::Via,
            route_target: "192.168.1.1".to_string(),
            domains: domains.into_iter().map(String::from).collect(),
            patterns: patterns.into_iter().map(String::from).collect(),
            static_routes: vec![],
            dns_protocol: Default::default(),
            cache_min_ttl: None,
            cache_max_ttl: None,
            cache_negative_ttl: None,
        }
    }

    fn exclusive_zone(name: &str, domains: Vec<&str>, patterns: Vec<&str>) -> ZoneConfig {
        ZoneConfig {
            mode: ZoneMode::Exclusive,
            ..test_zone(name, domains, patterns)
        }
    }

    #[test]
    fn test_domain_matcher() {
        let zone = test_zone("test", vec!["example.com"], vec![]);
        let matcher = ZoneMatcher::new(vec![zone]).unwrap();

        // Exact match
        assert!(matcher.find_zone("example.com").is_some());

        // Subdomain match
        assert!(matcher.find_zone("www.example.com").is_some());
        assert!(matcher.find_zone("api.prod.example.com").is_some());

        // No match
        assert!(matcher.find_zone("example.org").is_none());
        assert!(matcher.find_zone("notexample.com").is_none());
        assert!(matcher.find_zone("example.com.fake").is_none());
    }

    #[test]
    fn test_pattern_matcher() {
        let zone = test_zone("test", vec![], vec!["intra"]);
        let matcher = ZoneMatcher::new(vec![zone]).unwrap();

        // Pattern should match substring
        assert!(matcher.find_zone("app.dev.intra.corp").is_some());
        assert!(matcher.find_zone("intra.company.com").is_some());
        assert!(matcher.find_zone("my-intra-instance").is_some());

        // No match
        assert!(matcher.find_zone("github.com").is_none());
    }

    #[test]
    fn test_zone_precedence() {
        let zones = vec![
            ZoneConfig {
                route_target: "10.0.0.1".to_string(),
                ..test_zone("specific", vec!["api.example.com"], vec![])
            },
            ZoneConfig {
                route_target: "10.0.0.2".to_string(),
                ..test_zone("general", vec!["example.com"], vec![])
            },
        ];

        let matcher = ZoneMatcher::new(zones).unwrap();

        // Should match first zone (more specific)
        let zone = matcher.find_zone("api.example.com").unwrap();
        assert_eq!(zone.config.name, "specific");

        // Should match second zone
        let zone = matcher.find_zone("www.example.com").unwrap();
        assert_eq!(zone.config.name, "general");

        // Exact match on second zone
        let zone = matcher.find_zone("example.com").unwrap();
        assert_eq!(zone.config.name, "general");
    }

    #[test]
    fn test_regex_pattern_tld() {
        let zone = test_zone("ru-zone", vec![], vec![r"\.ru$"]);
        let matcher = ZoneMatcher::new(vec![zone]).unwrap();

        assert!(matcher.find_zone("example.ru").is_some());
        assert!(matcher.find_zone("mail.yandex.ru").is_some());
        assert!(matcher.find_zone("yandex.ru").is_some());

        assert!(matcher.find_zone("example.com").is_none());
        assert!(matcher.find_zone("ruble.com").is_none());
    }

    #[test]
    fn test_regex_pattern_prefix() {
        let zone = test_zone("corp-zone", vec![], vec!["^corp"]);
        let matcher = ZoneMatcher::new(vec![zone]).unwrap();

        assert!(matcher.find_zone("corp.internal.com").is_some());
        assert!(matcher.find_zone("corporate.net").is_some());

        assert!(matcher.find_zone("my.corp").is_none());
        assert!(matcher.find_zone("example.com").is_none());
    }

    #[test]
    fn test_exclusive_zone_basic() {
        let zone = exclusive_zone("vpn", vec!["google.com"], vec![]);
        let matcher = ZoneMatcher::new(vec![zone]).unwrap();

        // Excluded domain → no match
        assert!(matcher.find_zone("google.com").is_none());
        assert!(matcher.find_zone("www.google.com").is_none());

        // Everything else → matches exclusive zone
        assert_eq!(matcher.find_zone("example.com").unwrap().config.name, "vpn");
        assert_eq!(matcher.find_zone("github.com").unwrap().config.name, "vpn");
    }

    #[test]
    fn test_exclusive_zone_empty_exclusion_list() {
        let zone = exclusive_zone("catch-all", vec![], vec![]);
        let matcher = ZoneMatcher::new(vec![zone]).unwrap();

        // Empty exclusion list → matches everything
        assert_eq!(
            matcher.find_zone("anything.com").unwrap().config.name,
            "catch-all"
        );
        assert_eq!(
            matcher.find_zone("example.ru").unwrap().config.name,
            "catch-all"
        );
    }

    #[test]
    fn test_inclusive_then_exclusive_precedence() {
        let zones = vec![
            test_zone("corporate", vec!["internal.company.com"], vec![]),
            exclusive_zone("vpn-all", vec!["google.com"], vec![r"\.ru$"]),
        ];
        let matcher = ZoneMatcher::new(zones).unwrap();

        // Inclusive zone matched first
        assert_eq!(
            matcher
                .find_zone("internal.company.com")
                .unwrap()
                .config
                .name,
            "corporate"
        );

        // Not in inclusive zone, not excluded → exclusive catches it
        assert_eq!(
            matcher.find_zone("example.com").unwrap().config.name,
            "vpn-all"
        );

        // Excluded from exclusive zone → no match
        assert!(matcher.find_zone("google.com").is_none());
        assert!(matcher.find_zone("yandex.ru").is_none());
    }

    #[test]
    fn test_matched_zone_is_excluded() {
        let zone = ZoneConfig {
            static_routes: vec!["10.0.0.0/8".to_string(), "192.168.0.0/16".to_string()],
            ..exclusive_zone("vpn", vec!["google.com"], vec![])
        };
        let matcher = ZoneMatcher::new(vec![zone]).unwrap();

        let matched = matcher.find_zone("example.com").unwrap();

        // IPs in excluded ranges
        assert!(matched.is_excluded(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(matched.is_excluded(IpAddr::V4(Ipv4Addr::new(10, 255, 255, 255))));
        assert!(matched.is_excluded(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));

        // IPs not in excluded ranges
        assert!(!matched.is_excluded(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!matched.is_excluded(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));

        // IPv6 never excluded
        assert!(!matched.is_excluded(IpAddr::V6("::1".parse().unwrap())));
    }

    #[test]
    fn test_inclusive_zone_no_exclusions() {
        let zone = ZoneConfig {
            static_routes: vec!["172.16.0.0/12".to_string()],
            ..test_zone("corp", vec!["corp.example.com"], vec![])
        };
        let matcher = ZoneMatcher::new(vec![zone]).unwrap();

        let matched = matcher.find_zone("corp.example.com").unwrap();

        // Inclusive zones never exclude IPs (even if static_routes are present)
        assert!(!matched.is_excluded(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(!matched.is_excluded(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }

    #[test]
    fn test_invalid_regex_pattern() {
        let zone = test_zone("bad", vec![], vec!["[unclosed"]);
        let result = ZoneMatcher::new(vec![zone]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("bad"), "Error should mention zone name: {err}");
    }
}
