# Leshy

Split-tunnel DNS server that automatically manages IP routes based on DNS queries. Written in Rust.

## Why

When using split-tunnel VPNs, you need corporate domains to resolve via corporate DNS and route through the VPN tunnel, while everything else uses public DNS and default routing. Traditional solutions like `vpn-slice` hardcode IPs in `/etc/hosts`, which could break isolated networking patterns - i.e. Docker builds. Leshy solves this by running as a DNS server — all apps (including Docker) get correct routing transparently.

## How It Works

Leshy listens on `127.0.0.53:53`. For each DNS query it:

1. Matches the domain against configured zones (exact match, subdomain, or substring pattern)
2. Routes the query to the appropriate upstream DNS server
3. Parses A/AAAA records from the response
4. Adds IP routes via netlink (no `ip` command needed)

## Build

```bash
cargo build --release
sudo cp target/release/leshy /usr/local/bin/
```

## Configure

See [config.example.toml](config.example.toml) for a full example.

```toml
[server]
listen_address = "127.0.0.53:53"
default_upstream = ["8.8.8.8:53", "8.8.4.4:53"]
route_failure_mode = "fallback"  # or "servfail"
auto_reload = true               # reload config on file change

[[zones]]
name = "corporate"
dns_servers = ["10.0.0.2:53"]
route_type = "dev"                          # route via VPN tunnel device
route_target = "/run/vpn/corporate.dev"     # file containing device name (e.g. "tun0")
domains = ["internal.company.com", "git.company.com"]
patterns = ["corp"]                         # substring match

[[zones]]
name = "eu"
dns_servers = []                  # empty = use default_upstream
route_type = "via"                # route via static gateway
route_target = "192.168.169.1"
domains = ["example.com"]
patterns = []
```

**Route types:**

- `dev` — reads device name from file, routes via that device. Works with VPNs that connect/disconnect.
- `via` — routes through a fixed gateway IP.

**Domain matching:**

- `domains` — exact match, also matches all subdomains (e.g. `company.com` matches `git.company.com`)
- `patterns` — substring match anywhere in the queried name

### Composable Config

Zones can be split into separate files under `config.d/`:

```
/etc/leshy/
├── config.toml
└── config.d/
    ├── 10-corporate.toml
    └── 20-eu-vpn.toml
```

### Hot Reload

With `auto_reload = true`, Leshy watches the config file and reloads on changes. Invalid configs are rejected and the old config stays active. Routes for removed zones remain in the kernel table (safe for active connections).

## Run

Requires `CAP_NET_ADMIN` (routes) and `CAP_NET_BIND_SERVICE` (port 53):

```bash
sudo leshy /etc/leshy/config.toml

# Or with capabilities:
sudo setcap cap_net_admin,cap_net_bind_service+eip /usr/local/bin/leshy
leshy /etc/leshy/config.toml
```

### systemd

```ini
[Unit]
Description=Leshy split-tunnel DNS server
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/leshy /etc/leshy/config.toml
Restart=on-failure
CapabilityBoundingSet=CAP_NET_BIND_SERVICE CAP_NET_ADMIN
AmbientCapabilities=CAP_NET_BIND_SERVICE CAP_NET_ADMIN

[Install]
WantedBy=multi-user.target
```

### System DNS

Point `/etc/resolv.conf` at Leshy:

```
nameserver 127.0.0.53
```

### VPN Integration

Write the tunnel device name to the configured file when VPN connects:

```bash
echo "$TUNDEV" > /run/vpn/corporate.dev
```

## Development

```bash
make watch          # watch and auto-test on changes (requires entr)
cargo test          # unit tests
cargo clippy        # lint
cargo fmt           # format

# integration tests (requires root)
sudo cargo test --test integration_test -- --ignored
```

## Architecture

```
┌──────────────────────────────────────┐
│        Application / Docker          │
└───────────────┬──────────────────────┘
                │ DNS query
                ▼
┌──────────────────────────────────────┐
│  Leshy (127.0.0.53:53)              │
│                                      │
│  Zone Matcher → DNS Handler          │
│       domain/pattern matching        │
│       route to zone or default DNS   │
│                                      │
│  Route Manager (rtnetlink)           │
│       add routes via netlink         │
│       via/dev route types            │
└──────────────────────────────────────┘
```

## License

MIT
