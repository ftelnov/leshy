# Leshy - Development Guide

## Project Overview

Leshy is a DNS server for VPN and network routing. It intercepts DNS queries, forwards them to zone-specific or default upstream servers, and adds IP routes for resolved addresses through VPN tunnels or gateways.

**Stack:** Rust (2021 edition), Hickory DNS, Tokio, rtnetlink (Linux), TOML config.

## Local Workflow (Before Deploying)

### 1. Run linter and unit tests

```bash
make test
```

This runs (in order):
- `cargo fmt -- --check` — formatting
- `cargo clippy --all-targets --all-features -- -D warnings` — lints
- `cargo test` — unit tests + config validation integration test

All three must pass before proceeding.

### 2. Run integration tests (requires Docker)

```bash
make integration-test
```

This spins up:
- `public-dns` (CoreDNS at 172.28.0.10) — serves known public A records
- `corporate-dns` (CoreDNS at 172.28.0.20) — serves internal corporate records
- `test-runner` (Debian + leshy binary + pytest) — runs 11 end-to-end tests

Tests cover: DNS forwarding, zone-specific DNS, route via gateway, route via device, fallback on route failure, pattern matching, VPN reconnect lifecycle, static routes, DNS response caching, upstream failover, all-upstreams-fail.

### 3. Fix any issues

If formatting fails: `make fmt` then re-run `make test`.

### Full pre-deploy checklist

```bash
make test              # lint + unit tests
make integration-test  # Docker e2e tests
```

Both must pass before pushing.

## Project Structure

```
src/
  config.rs          — Config parsing (TOML, zones, dns_servers)
  dns/
    handler.rs       — DNS request handler, upstream forwarding, caching
    cache.rs         — DNS response cache
    mod.rs           — DNS server setup
  routing/
    mod.rs           — Route manager (add/remove routes per zone)
    linux.rs         — Linux rtnetlink route operations
    macos.rs         — macOS /sbin/route operations
  reload.rs          — Hot-reload config watcher
  zones/
    matcher.rs       — Domain/pattern matching for zones

tests/
  integration_test.rs      — Config validation test (no network/root needed)
  composable_config_test.rs — Config.d directory merging tests
  hot_reload_test.rs       — Config hot-reload tests
  fixtures/                — Test config fixtures
  docker/                  — Docker integration tests
    docker-compose.yml     — Three-service compose setup
    Dockerfile.test        — Multi-stage: rust builder + debian runner
    coredns/               — CoreDNS Corefiles for public/corporate DNS
    configs/               — Leshy TOML configs per test scenario
    conftest.py            — Pytest fixtures (leshy, dns_query, etc.)
    test_integration.py    — 11 integration tests
```

## Key Commands

| Command | Description |
|---------|-------------|
| `make test` | fmt + clippy + unit tests |
| `make integration-test` | Docker e2e tests |
| `make build` | Build release binary |
| `make fmt` | Auto-format code |
| `make clippy` | Run clippy lints |
| `make run` | Run with example config (needs sudo) |
| `make watch` | Watch + auto-test on changes |

## CI Pipeline

Four jobs in `.github/workflows/ci.yml`:

- **test** — `cargo fmt + clippy + test` on ubuntu + macos
- **integration-linux** — Docker compose tests on ubuntu
- **integration-macos** — Native tests on macOS (CoreDNS via brew, loopback aliases, sudo route)
- **build** — Release build + artifact upload on ubuntu + macos

### Verifying CI (push-until-green workflow)

After pushing changes, monitor CI and fix failures iteratively:

```bash
# 1. Push your changes
git push

# 2. Watch the triggered run (blocks until complete)
gh run watch $(gh run list --limit 1 --json databaseId -q '.[0].databaseId') --exit-status

# 3. If it fails, inspect the failed job logs
gh run view <run-id> --log-failed

# 4. Fix, commit, push, repeat until all 6 jobs pass
```

Common CI failures and fixes:

- **`cargo fmt` diff** — Local rustfmt version may differ from CI's. Run `cargo fmt` and verify with `cargo fmt -- --check` before pushing. Format ALL files, not just the ones you touched.
- **macOS pip `externally-managed-environment`** — Use a venv: `python3 -m venv /tmp/venv && /tmp/venv/bin/pip install ...`
- **CoreDNS listen address** — CoreDNS doesn't have a `-dns.addr` flag. Use `bind <ip>` directive inside the Corefile instead.
- **macOS route table format** — `netstat -rn` trims trailing `.0` octets (e.g. `10.99/24` instead of `10.99.0.0/24`). Write assertions that match both formats.

After all jobs are green, squash any fix-up commits into the original and force push:

```bash
git reset --soft <original-commit>
git commit --amend --no-edit
git push --force
```

## Config Format

See `config.example.toml` for full reference. Key concepts:
- `[server]` — listen address, default upstream, route failure mode, cache settings
- `[[zones]]` — name, dns_servers, route_type (via/dev), route_target, domains, patterns, static_routes
- Zone DNS can be simple (`["ip:port"]`) or rich (`[{ address, cache_min_ttl }]`)
