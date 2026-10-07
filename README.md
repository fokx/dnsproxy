# dnsproxy

A fast, robust, and resilient DNS proxy server written in Rust.

## Features

- **Multi-Protocol Client Support**: Concurrent UDP and TCP DNS listeners (RFC 1035 / RFC 7766) on configurable IP addresses and ports.
- **Upstream Forwarding Strategies**:
  - `parallel`: Queries all configured upstreams concurrently, returning the fastest valid response.
  - `round_robin` / `load_balance`: Sequential failover rotating through upstream servers.
- **DNS-over-HTTPS (DoH)**: Native RFC 8484 DoH query support with persistent HTTP connection pooling (`reqwest`) and keep-alive optimization.
- **Per-Upstream SOCKS5 Proxying**: Route individual DoH upstreams through separate SOCKS5 proxies with optional username and password authentication.
- **Plain UDP DNS Support**: Resolve domain names or IP-based upstreams directly or using a bootstrap resolver.
- **Dual-Layer Caching**:
  - In-memory `(Name, RecordType)` cache respecting DNS record TTLs.
  - Size-based LRU/expiry pruning (`max_cache_size`, `max_cache_ttl`).
  - Atomic disk persistence across server restarts using atomic temporary files.
- **IPv6 (AAAA) Filtering**: Optional stripping of AAAA records with wildcard-enabled domain whitelist (`*.example.com`).
- **Concurrency Rate Limiting**: Semaphore-based concurrency limiter to prevent server overload.
- **Modern Rust & Ergonomics**: Built on Rust Edition 2024, fully modularized codebase, comprehensive unit test suite, and clean `clippy` validation.

---

## Quick Start with Cargo

### 1. Install Binary (`dnsproxy`)

```bash
cargo install dnsproxy
```

This installs the **`dnsproxy`** binary into `$HOME/.cargo/bin/`.

### 2. Run

```bash
# Run with default config.toml
dnsproxy

# Or run specifying a custom configuration file
dnsproxy --config /path/to/config.toml
```

### 3. Test DNS Resolution

```bash
# Query via UDP
dig @127.0.0.1 -p 53000 example.com A

# Query via TCP
dig @127.0.0.1 -p 53000 example.com A +tcp
```

---

## Dependency Migration

All dependencies have been migrated from unreleased internal workspace versions to the latest published releases on [crates.io](https://crates.io):

| Dependency | Original Branch | `dnsproxy` (Latest) | Notes |
|------------|-----------------|---------------------|-------|
| `hickory-proto` | `0.27.0-alpha.1` (local) | `0.26.3` | Core DNS protocol parsing & encoding |
| `hickory-resolver` | `0.27.0-alpha.1` (local) | `0.26.3` | Bootstrap resolver for upstream hostnames |
| `hickory-net` | `0.27.0-alpha.1` (local) | `0.26.3` | Async networking primitives |
| `tokio` | `1.36` | `1.53.2` | Async runtime (`rt-multi-thread`, `net`, `sync`) |
| `reqwest` | `0.13.1` | `0.13.5` | HTTP client with `rustls` and `socks` features |
| `clap` | `4.0` | `4.6.7` | CLI argument parsing with `derive` |
| `serde` | `1.0` | `1.0.229` | Configuration & cache serialization |
| `toml` | `1.0.1` | `1.1.6` | TOML configuration & persistent disk cache |
| `tracing` / `tracing-subscriber` | `0.1.30` / `0.3.10` | `0.1.44` / `0.3.23` | Structured logging with dynamic log level filtering |
| `futures-util` | `0.3.5` | `0.3.34` | Async combinators (`select_all`) |

---

## Configuration

`dnsproxy` reads configuration from a TOML file (`config.toml` by default, or specified via `--config / -c`).

```toml
# Log level: trace, debug, info, warn, error
log_level = "info"

# DNS Bootstrap resolvers for resolving upstream domains
bootstrap = [
    "1.1.1.1",
    "9.9.9.9",
    "8.8.8.8:53"
]

# Listen addresses & ports
listen_addrs = ["127.0.0.1"]
listen_ports = [53000]

# Upstream resolution strategy: "parallel" or "round_robin"
upstream_mode = "parallel"

# Upstream query timeout
timeout = "10s"

# Configured upstream servers
upstreams = [
    "https://1.1.1.1/dns-query",
    "https://9.9.9.9/dns-query",
    # Route through a SOCKS5 proxy:
    # { url = "https://8.8.4.4/dns-query", socks5 = "socks5://127.0.0.1:1080" },
    # Authenticated SOCKS5 proxy:
    # { url = "https://8.8.4.4/dns-query", socks5 = "socks5://127.0.0.1:1080", username = "user", password = "password" },
]

# Cache configuration
cache = true
cache_file = "/tmp/dnsproxy-cache.toml"
max_cache_size = "50M"
max_cache_ttl = "1d"

# Concurrency rate limit (0 to disable)
ratelimit = 0

# IPv6 (AAAA) filtering
ipv6_disabled = false
ipv6_whitelist = [
    "example.org",
    "*.example.com",
]
```

---

## Deployment

### Option 1: Systemd Service (Linux)

A production-ready systemd unit file is available at [`deployment/dnsproxy.service`](deployment/dnsproxy.service). It is hardened with `CAP_NET_BIND_SERVICE` to allow binding to privileged port 53 without running as root.

```bash
# 1. Build and copy binary
cargo build --release
sudo cp target/release/dnsproxy /usr/local/bin/

# 2. Create unprivileged system user
sudo useradd -r -s /bin/false dnsproxy

# 3. Create directories and config
sudo mkdir -p /etc/dnsproxy /var/cache/dnsproxy
sudo cp config.example.toml /etc/dnsproxy/config.toml
sudo chown -R dnsproxy:dnsproxy /etc/dnsproxy /var/cache/dnsproxy

# 4. Install systemd service
sudo cp deployment/dnsproxy.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now dnsproxy

# 5. Check status and logs
sudo systemctl status dnsproxy
sudo journalctl -u dnsproxy -f
```

---

### Option 2: Docker & Docker Compose

#### Using Docker:
```bash
# Build image
docker build -t dnsproxy:latest .

# Run container
docker run -d \
  --name dnsproxy \
  --restart unless-stopped \
  -p 53000:53000/udp \
  -p 53000:53000/tcp \
  -v $(pwd)/config.toml:/etc/dnsproxy/config.toml:ro \
  dnsproxy:latest
```

#### Using Docker Compose:
```bash
docker compose up -d
docker compose logs -f
```

---

## Publishing to Crates.io

To publish this package to [crates.io](https://crates.io):

1. **Verify package files and build**:
   ```bash
   cargo package --allow-dirty
   ```

2. **Dry run publish**:
   ```bash
   cargo publish --dry-run
   ```

3. **Publish to crates.io**:
   ```bash
   cargo login <YOUR_CRATES_IO_TOKEN>
   cargo publish
   ```

---

## Makefile Shortcuts

A [`Makefile`](Makefile) is provided for common development and deployment workflows:

```bash
make build         # Build debug binary
make release       # Build optimized release binary
make test          # Run unit tests
make check         # Run clippy & cargo check
make run           # Run binary locally with config.toml
make install       # Install dnsproxy binary to ~/.cargo/bin
make package       # Create and verify crates.io package
make check-publish # Dry-run crates.io publish
make publish       # Publish to crates.io
make docker        # Build Docker image
make docker-run    # Start docker compose container
```

---

## License

Licensed under the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or [LICENSE](LICENSE)).
