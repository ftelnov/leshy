"""Integration tests for leshy DNS server.

Runs in Docker (Linux) or natively on macOS with:
- public-dns at 172.28.0.10 (serves known public records)
- corporate-dns at 172.28.0.20 (serves internal records)
- Route manipulation via CAP_NET_ADMIN (Linux) or sudo (macOS)

Each test uses unique domains/IPs to avoid route conflicts between tests.
"""

import os
import signal
import time

import dns.resolver
import pytest


def test_basic_dns_forwarding(leshy, dns_query):
    """Forward to public upstream, get known answers."""
    leshy("basic.toml")

    answer = dns_query("example.com")
    ips = [rr.address for rr in answer]
    assert "93.184.216.34" in ips

    answer2 = dns_query("google.com")
    ips2 = [rr.address for rr in answer2]
    assert "142.250.80.46" in ips2


def test_zone_specific_dns(leshy, dns_query):
    """Corporate zone -> corporate-dns, default -> public-dns."""
    leshy("zone-dns.toml")

    # Corporate domain resolved by corporate-dns
    answer = dns_query("internal.company.com")
    ips = [rr.address for rr in answer]
    assert "10.0.1.1" in ips

    answer2 = dns_query("git.company.com")
    ips2 = [rr.address for rr in answer2]
    assert "10.0.1.2" in ips2

    # Non-corporate domain falls through to public-dns
    answer3 = dns_query("google.com")
    ips3 = [rr.address for rr in answer3]
    assert "142.250.80.46" in ips3


def test_route_via_gateway(leshy, dns_query, get_routes):
    """After resolving cloudflare.com, route added via 172.28.0.1."""
    leshy("multi-zone.toml")

    dns_query("cloudflare.com")
    time.sleep(0.5)

    routes = get_routes()
    assert "104.16.132.229" in routes
    assert "172.28.0.1" in routes


def test_route_via_device(leshy, dns_query, get_routes, dummy_interface):
    """Improvised VPN: route added via dummy-vpn interface."""
    leshy("corporate-vpn.toml")

    # Use wiki.company.com (10.0.1.3) — unique IP not queried by other tests
    answer = dns_query("wiki.company.com")
    ips = [rr.address for rr in answer]
    assert "10.0.1.3" in ips

    time.sleep(0.5)
    routes = get_routes()
    assert dummy_interface in routes
    assert "10.0.1.3" in routes


def test_fallback_on_route_failure(leshy, dns_query):
    """DNS succeeds despite non-existent gateway (fallback mode)."""
    leshy("fallback.toml")

    # Route to 10.255.255.254 will fail, but DNS should still return an answer
    answer = dns_query("example.com")
    ips = [rr.address for rr in answer]
    assert "93.184.216.34" in ips


def test_pattern_matching(leshy, dns_query):
    """Patterns like 'corp' match service.corp.internal."""
    leshy("zone-dns.toml")

    # "corp" pattern should match service.corp.internal
    answer = dns_query("service.corp.internal")
    ips = [rr.address for rr in answer]
    assert "10.0.1.10" in ips


def test_vpn_reconnect(leshy, dns_query, get_routes, dummy_interface):
    """Device file lifecycle: missing -> created -> removed."""
    dev_file = "/tmp/vpn.dev"

    # Remove device file to simulate VPN disconnected
    if os.path.exists(dev_file):
        os.remove(dev_file)

    leshy("corporate-vpn.toml")

    # Query should succeed even without device file (fallback mode)
    answer1 = dns_query("example.net")
    assert len(answer1) > 0

    # Simulate VPN connect: write device file
    with open(dev_file, "w") as f:
        f.write(dummy_interface)

    # Query corporate domain — use jira.company.com (10.0.1.5), unique IP
    answer2 = dns_query("jira.company.com")
    ips = [rr.address for rr in answer2]
    assert "10.0.1.5" in ips

    time.sleep(0.5)
    routes = get_routes()
    assert dummy_interface in routes
    assert "10.0.1.5" in routes

    # Simulate VPN disconnect: remove device file
    os.remove(dev_file)

    # Public queries should still work
    answer3 = dns_query("example.org")
    assert len(answer3) > 0


def test_static_routes(leshy, get_routes):
    """Static route 10.99.0.0/24 added on startup."""
    leshy("static-routes.toml")
    time.sleep(1)

    routes = get_routes()
    # Linux: "10.99.0.0/24", macOS netstat: "10.99/24" (trims trailing .0 octets)
    assert "10.99" in routes


def test_cache_hit(leshy, dns_query):
    """Repeated query returns cached response, verified via log output."""
    proc = leshy("cache.toml", env_extra={"RUST_LOG": "debug"})

    # First query: cache miss, forwarded to upstream
    answer1 = dns_query("www.example.com")
    ips1 = [rr.address for rr in answer1]
    assert "93.184.216.34" in ips1

    # Second query: should hit cache
    answer2 = dns_query("www.example.com")
    ips2 = [rr.address for rr in answer2]
    assert "93.184.216.34" in ips2

    # Stop process to flush output buffers, then verify cache hit in logs
    proc.send_signal(signal.SIGTERM)
    proc.wait(timeout=5)

    with open(proc.log_path) as f:
        logs = f.read()
    assert "Cache hit" in logs


def test_upstream_failover(leshy, dns_query):
    """First upstream unreachable, falls over to second and resolves."""
    leshy("upstream-failover.toml")

    # 172.28.0.99 is unreachable (5s timeout); leshy should fail over to 172.28.0.10
    # Set per-query timeout > 5s so the client waits for the failover to complete
    answer = dns_query("docker.io", lifetime=20, timeout=10)
    ips = [rr.address for rr in answer]
    assert "185.199.108.153" in ips


def test_servfail_failover(leshy, dns_query):
    """First upstream returns REFUSED, falls over to second and resolves."""
    leshy("servfail-failover.toml")

    # 172.28.0.30 returns REFUSED; leshy should fail over to 172.28.0.10
    # Use higher lifetime to tolerate CI variability (failover itself is instant)
    answer = dns_query("github.com", lifetime=15, timeout=10)
    ips = [rr.address for rr in answer]
    assert "140.82.121.4" in ips


def test_all_upstreams_fail(leshy, dns_query):
    """All upstreams unreachable returns SERVFAIL."""
    leshy("all-fail.toml")

    with pytest.raises((dns.resolver.NoNameservers, dns.resolver.LifetimeTimeout)):
        dns_query("anything.test", lifetime=10)


def test_route_aggregation(leshy, dns_query, get_routes):
    """With route_aggregation_prefix=24, route shows /24 network instead of /32."""
    leshy("aggregation.toml")

    dns_query("cloudflare.com")
    time.sleep(0.5)

    routes = get_routes()
    # 104.16.132.229 should be aggregated into 104.16.132.0/24
    # Linux: "104.16.132.0/24", macOS netstat: "104.16.132/24" (trims trailing .0 octets)
    assert "104.16.132.0/24" in routes or "104.16.132/24" in routes
    assert "172.28.0.1" in routes


def test_exclusive_zone(leshy, dns_query, get_routes):
    """Exclusive zone routes everything except excluded domains/patterns."""
    leshy("exclusive-zone.toml")

    # Corporate domain → resolved via corporate DNS (inclusive zone first)
    answer = dns_query("internal.company.com")
    ips = [rr.address for rr in answer]
    assert "10.0.1.1" in ips

    # Non-excluded domain → exclusive zone catches it, route added
    dns_query("example.de")
    time.sleep(0.5)
    routes = get_routes()
    assert "93.184.216.100" in routes
    assert "172.28.0.1" in routes

    # Excluded domain (google.com) → resolves but NO route
    dns_query("google.com")
    time.sleep(0.5)
    routes = get_routes()
    assert "142.250.80.46" not in routes

    # Excluded wildcard (*.ru) → resolves but NO route
    dns_query("yandex.ru")
    time.sleep(0.5)
    routes = get_routes()
    assert "77.88.55.242" not in routes


def test_wildcard_pattern(leshy, dns_query, get_routes):
    """Wildcard pattern *.ru matches .ru domains."""
    leshy("wildcard-pattern.toml")

    # yandex.ru matches *.ru → route added
    dns_query("yandex.ru")
    time.sleep(0.5)
    routes = get_routes()
    assert "77.88.55.242" in routes
    assert "172.28.0.1" in routes

    # google.com doesn't match *.ru → no route
    dns_query("google.com")
    time.sleep(0.5)
    routes = get_routes()
    assert "142.250.80.46" not in routes
