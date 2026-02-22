use crate::config::{ZoneConfig, ZoneMode};
use regex::Regex;
use std::sync::Arc;

pub struct ZoneMatcher {
    zones: Vec<ZoneEntry>,
}

struct ZoneEntry {
    config: Arc<ZoneConfig>,
    domain_matchers: Vec<DomainMatcher>,
    pattern_regexes: Vec<Regex>,
}

struct DomainMatcher {
    domain: String,
    // Pattern for exact match: ^domain$
    exact_regex: Regex,
    // Pattern for subdomain match: ^.*\.domain$
    subdomain_regex: Regex,
}

/// Compile a pattern string into a regex.
/// If the pattern contains `*`, it is treated as a glob wildcard (`*.ru` → `^.*\.ru$`).
/// Otherwise, it uses legacy substring matching (`intra` → `.*intra.*`).
fn compile_pattern(pattern: &str) -> anyhow::Result<Regex> {
    let regex_str = if pattern.contains('*') {
        // Glob-style: split on *, escape each segment, join with .*
        let parts: Vec<String> = pattern.split('*').map(regex::escape).collect();
        format!("^{}$", parts.join(".*"))
    } else {
        // Legacy substring match (backward compatible)
        let escaped = regex::escape(pattern);
        format!(".*{escaped}.*")
    };
    Ok(Regex::new(&regex_str)?)
}

impl ZoneMatcher {
    pub fn new(zones: Vec<ZoneConfig>) -> anyhow::Result<Self> {
        let mut zone_entries = Vec::new();

        for zone in zones {
            let mut domain_matchers = Vec::new();
            for domain in &zone.domains {
                domain_matchers.push(DomainMatcher::new(domain)?);
            }

            let mut pattern_regexes = Vec::new();
            for pattern in &zone.patterns {
                pattern_regexes.push(compile_pattern(pattern)?);
            }

            zone_entries.push(ZoneEntry {
                config: Arc::new(zone),
                domain_matchers,
                pattern_regexes,
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
        for matcher in &zone.domain_matchers {
            if matcher.matches(qname) {
                tracing::debug!(
                    zone = zone.config.name,
                    domain = matcher.domain,
                    qname = qname,
                    "Domain match"
                );
                return true;
            }
        }

        for pattern_regex in &zone.pattern_regexes {
            if pattern_regex.is_match(qname) {
                tracing::debug!(
                    zone = zone.config.name,
                    pattern = pattern_regex.as_str(),
                    qname = qname,
                    "Pattern match"
                );
                return true;
            }
        }

        false
    }
}

impl DomainMatcher {
    fn new(domain: &str) -> anyhow::Result<Self> {
        // Escape special regex characters
        let escaped = regex::escape(domain);

        // Exact match: ^domain$
        let exact_pattern = format!("^{escaped}$");
        let exact_regex = Regex::new(&exact_pattern)?;

        // Subdomain match: ^.*\.domain$
        let subdomain_pattern = format!(r"^.*\.{escaped}$");
        let subdomain_regex = Regex::new(&subdomain_pattern)?;

        Ok(Self {
            domain: domain.to_string(),
            exact_regex,
            subdomain_regex,
        })
    }

    fn matches(&self, qname: &str) -> bool {
        self.exact_regex.is_match(qname) || self.subdomain_regex.is_match(qname)
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
        let matcher = DomainMatcher::new("example.com").unwrap();

        // Exact match
        assert!(matcher.matches("example.com"));

        // Subdomain match
        assert!(matcher.matches("www.example.com"));
        assert!(matcher.matches("api.prod.example.com"));

        // No match
        assert!(!matcher.matches("example.org"));
        assert!(!matcher.matches("notexample.com"));
        assert!(!matcher.matches("example.com.fake"));
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
    fn test_wildcard_pattern_star_dot_ru() {
        let zone = test_zone("ru-zone", vec![], vec!["*.ru"]);
        let matcher = ZoneMatcher::new(vec![zone]).unwrap();

        assert!(matcher.find_zone("example.ru").is_some());
        assert!(matcher.find_zone("mail.yandex.ru").is_some());
        assert!(matcher.find_zone("yandex.ru").is_some());

        assert!(matcher.find_zone("example.com").is_none());
        assert!(matcher.find_zone("ruble.com").is_none());
    }

    #[test]
    fn test_wildcard_pattern_prefix() {
        let zone = test_zone("corp-zone", vec![], vec!["corp*"]);
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
            exclusive_zone("vpn-all", vec!["google.com"], vec!["*.ru"]),
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
}
