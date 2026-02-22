use crate::config::{ZoneConfig, ZoneMode};
use regex::RegexSet;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug)]
pub struct ZoneMatcher {
    zones: Vec<ZoneEntry>,
}

#[derive(Debug)]
struct ZoneEntry {
    config: Arc<ZoneConfig>,
    domain_set: HashSet<String>,
    pattern_set: RegexSet,
}

impl ZoneMatcher {
    pub fn new(zones: Vec<ZoneConfig>) -> anyhow::Result<Self> {
        let mut zone_entries = Vec::new();

        for zone in zones {
            let domain_set: HashSet<String> =
                zone.domains.iter().map(|d| d.to_lowercase()).collect();

            let pattern_set = RegexSet::new(&zone.patterns).map_err(|e| {
                anyhow::anyhow!("Zone '{}': invalid regex pattern: {}", zone.name, e)
            })?;

            zone_entries.push(ZoneEntry {
                config: Arc::new(zone),
                domain_set,
                pattern_set,
            });
        }

        Ok(Self {
            zones: zone_entries,
        })
    }

    /// Find the first zone that matches the given query name
    pub fn find_zone(&self, qname: &str) -> Option<Arc<ZoneConfig>> {
        // Normalize: remove trailing dot if present
        let qname = qname.trim_end_matches('.');

        for zone in &self.zones {
            let any_match = Self::matches_zone(zone, qname);

            match zone.config.mode {
                ZoneMode::Inclusive => {
                    if any_match {
                        return Some(Arc::clone(&zone.config));
                    }
                }
                ZoneMode::Exclusive => {
                    if !any_match {
                        tracing::debug!(
                            zone = zone.config.name,
                            qname = qname,
                            "Exclusive zone match (not excluded)"
                        );
                        return Some(Arc::clone(&zone.config));
                    }
                    // Matched exclusion list — fall through to next zone
                    tracing::debug!(
                        zone = zone.config.name,
                        qname = qname,
                        "Excluded from exclusive zone"
                    );
                }
            }
        }

        tracing::debug!(qname = qname, "No zone match, using default");
        None
    }

    /// Check whether a domain matches any domain or pattern in the zone
    fn matches_zone(zone: &ZoneEntry, qname: &str) -> bool {
        // Check domain set: walk suffix labels
        let lower = qname.to_lowercase();
        let mut remaining = lower.as_str();
        loop {
            if zone.domain_set.contains(remaining) {
                tracing::debug!(
                    zone = zone.config.name,
                    domain = remaining,
                    qname = qname,
                    "Domain match"
                );
                return true;
            }
            // Strip the first label
            match remaining.find('.') {
                Some(pos) => remaining = &remaining[pos + 1..],
                None => break,
            }
        }

        // Check pattern set (single RegexSet call)
        if zone.pattern_set.is_match(qname) {
            tracing::debug!(zone = zone.config.name, qname = qname, "Pattern match");
            return true;
        }

        false
    }
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
        assert_eq!(zone.name, "specific");

        // Should match second zone
        let zone = matcher.find_zone("www.example.com").unwrap();
        assert_eq!(zone.name, "general");

        // Exact match on second zone
        let zone = matcher.find_zone("example.com").unwrap();
        assert_eq!(zone.name, "general");
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
        assert_eq!(matcher.find_zone("example.com").unwrap().name, "vpn");
        assert_eq!(matcher.find_zone("github.com").unwrap().name, "vpn");
    }

    #[test]
    fn test_exclusive_zone_empty_exclusion_list() {
        let zone = exclusive_zone("catch-all", vec![], vec![]);
        let matcher = ZoneMatcher::new(vec![zone]).unwrap();

        // Empty exclusion list → matches everything
        assert_eq!(matcher.find_zone("anything.com").unwrap().name, "catch-all");
        assert_eq!(matcher.find_zone("example.ru").unwrap().name, "catch-all");
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
            matcher.find_zone("internal.company.com").unwrap().name,
            "corporate"
        );

        // Not in inclusive zone, not excluded → exclusive catches it
        assert_eq!(matcher.find_zone("example.com").unwrap().name, "vpn-all");

        // Excluded from exclusive zone → no match
        assert!(matcher.find_zone("google.com").is_none());
        assert!(matcher.find_zone("yandex.ru").is_none());
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
