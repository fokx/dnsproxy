# ==========================================
# Build Stage
# ==========================================
FROM rust:1-bookworm AS builder

WORKDIR /usr/src/dnsproxy

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code and config
COPY src ./src
COPY config.example.toml ./

# Build optimized release binary
RUN cargo build --release --locked --bin dnsproxy

# ==========================================
# Runtime Stage
# ==========================================
FROM debian:bookworm-slim

# Install CA certificates for DoH (HTTPS) upstreams
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates tzdata && \
    rm -rf /var/lib/apt/lists/*

# Create unprivileged system user
RUN groupadd -r -g 1000 dnsproxy && \
    useradd -r -u 1000 -g dnsproxy -s /sbin/nologin dnsproxy

# Prepare configuration and cache directories
RUN mkdir -p /etc/dnsproxy /var/cache/dnsproxy && \
    chown -R dnsproxy:dnsproxy /etc/dnsproxy /var/cache/dnsproxy

# Copy binary from builder
COPY --from=builder /usr/src/dnsproxy/target/release/dnsproxy /usr/local/bin/dnsproxy

# Copy default config template
COPY config.example.toml /etc/dnsproxy/config.toml

# DNS standard ports
EXPOSE 53/udp 53/tcp

# Run as non-root user
USER dnsproxy

ENTRYPOINT ["/usr/local/bin/dnsproxy"]
CMD ["--config", "/etc/dnsproxy/config.toml"]
