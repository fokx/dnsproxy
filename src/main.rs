use clap::Parser;
use dnsproxy::config::Config;
use dnsproxy::proxy::DnsProxy;
use std::path::PathBuf;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "dnsproxy",
    version,
    about = "A high-performance, resilient DNS proxy server supporting DoH, parallel querying, connection pooling, and SOCKS5 proxies."
)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();

    // Early config load to determine configured log level if RUST_LOG is not explicitly set
    let config = Config::from_file(&args.config)?;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_level(true)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .init();

    info!("Starting dnsproxy v{}", env!("CARGO_PKG_VERSION"));
    info!("Configuration path: {}", args.config.display());
    info!(
        "Listening addresses: {:?}, ports: {:?}",
        config.listen_addrs, config.listen_ports
    );
    info!("Upstream mode: {:?}", config.upstream_mode);
    info!("Configured upstreams: {}", config.upstreams.len());
    info!("Cache enabled: {}", config.cache);
    info!("IPv6 disabled: {}", config.ipv6_disabled);

    let proxy = DnsProxy::new(config).await?;
    proxy.run().await?;

    Ok(())
}
