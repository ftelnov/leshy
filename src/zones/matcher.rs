use crate::config::ZoneConfig;
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
                // Escape regex special chars, then create substring match pattern
                let escaped = regex::escape(pattern);
                let pattern = format!(".*{escaped}.*");
                pattern_regexes.push(Regex::new(&pattern)?);
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
            // Try domain matchers first (more specific)
            for matcher in &zone.domain_matchers {
                if matcher.matches(qname) {
                    tracing::debug!(
                        zone = zone.config.name,
                        domain = matcher.domain,
                        qname = qname,
                        "Domain match"
                    );
                    return Some(Arc::clone(&zone.config));
                }
            }

            // Try pattern matchers
            for pattern_regex in &zone.pattern_regexes {
                if pattern_regex.is_match(qname) {
                    tracing::debug!(
                        zone = zone.config.name,
                        pattern = pattern_regex.as_str(),
                        qname = qname,
                        "Pattern match"
                    );
                    return Some(Arc::clone(&zone.config));
                }
            }
        }

        tracing::debug!(qname = qname, "No zone match, using default");
        None
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
        let zone = ZoneConfig {
            name: "test".to_string(),
            dns_servers: vec![],
            route_type: crate::config::RouteType::Via,
            route_target: "192.168.1.1".to_string(),
            domains: vec![],
            patterns: vec!["intra".to_string()],
            static_routes: vec![],
        };

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
                name: "specific".to_string(),
                dns_servers: vec![],
                route_type: crate::config::RouteType::Via,
                route_target: "10.0.0.1".to_string(),
                domains: vec!["api.example.com".to_string()],
                patterns: vec![],
                static_routes: vec![],
            },
            ZoneConfig {
                name: "general".to_string(),
                dns_servers: vec![],
                route_type: crate::config::RouteType::Via,
                route_target: "10.0.0.2".to_string(),
                domains: vec!["example.com".to_string()],
                patterns: vec![],
                static_routes: vec![],
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
}
