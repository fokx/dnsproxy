use crate::config::Upstream;
use reqwest::Client as HttpClient;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::debug;

/// Connection pool for HTTP/HTTPS upstream clients.
/// Reuses persistent TLS/TCP connections per upstream to avoid handshake overhead.
#[derive(Clone)]
pub struct ConnectionPool {
    clients: Arc<RwLock<HashMap<String, HttpClient>>>,
}

impl ConnectionPool {
    pub fn new() -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get_client(
        &self,
        upstream: &Upstream,
    ) -> Result<HttpClient, Box<dyn std::error::Error + Send + Sync>> {
        let key = self.make_key(upstream);

        // Try reading existing client
        {
            let clients = self.clients.read().await;
            if let Some(client) = clients.get(&key) {
                return Ok(client.clone());
            }
        }

        // Build new client
        let mut client_builder = HttpClient::builder()
            .timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(90));

        // Configure SOCKS5 proxy if specified
        if let Some((socks5_addr, username, password)) = upstream.socks5_config() {
            let proxy_url = if socks5_addr.starts_with("socks5://") {
                socks5_addr.to_string()
            } else {
                format!("socks5://{}", socks5_addr)
            };

            let mut proxy = reqwest::Proxy::all(&proxy_url)?;

            if let (Some(user), Some(pass)) = (username, password) {
                proxy = proxy.basic_auth(user, pass);
            }

            client_builder = client_builder.proxy(proxy);
            debug!(
                "Created pooled client with socks5 proxy {} for upstream {}",
                proxy_url,
                upstream.url()
            );
        }

        let client = client_builder.build()?;

        // Store in pool
        let mut clients = self.clients.write().await;
        clients.insert(key, client.clone());

        Ok(client)
    }

    fn make_key(&self, upstream: &Upstream) -> String {
        match upstream {
            Upstream::Simple(url) => url.clone(),
            Upstream::WithProxy {
                url,
                socks5,
                username,
                ..
            } => {
                format!("{}|{}|{}", url, socks5, username.as_deref().unwrap_or(""))
            }
        }
    }
}

impl Default for ConnectionPool {
    fn default() -> Self {
        Self::new()
    }
}
