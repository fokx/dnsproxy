use crate::cache::DnsCache;
use crate::config::{Config, UpstreamMode};
use crate::pool::ConnectionPool;
use crate::upstream::query_upstream;
use hickory_net::runtime::TokioRuntimeProvider;
use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::{Name, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};
use hickory_resolver::TokioResolver;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{RwLock, Semaphore};
use tokio::time::timeout;
use tracing::{debug, error, info, warn};

#[derive(Clone)]
pub struct DnsProxy {
    config: Config,
    system_resolver: Option<TokioResolver>,
    cache: DnsCache,
    round_robin_index: Arc<RwLock<usize>>,
    rate_limiter: Option<Arc<Semaphore>>,
    connection_pool: ConnectionPool,
}

impl DnsProxy {
    pub async fn new(config: Config) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let system_resolver = if !config.bootstrap.is_empty() {
            match TokioResolver::builder(TokioRuntimeProvider::default()) {
                Ok(builder) => Some(builder.build()?),
                Err(e) => {
                    warn!("Failed to create resolver: {}, using system default", e);
                    None
                }
            }
        } else {
            match TokioResolver::builder(TokioRuntimeProvider::default()) {
                Ok(builder) => Some(builder.build()?),
                Err(e) => {
                    warn!("Failed to create resolver: {}", e);
                    None
                }
            }
        };

        let rate_limiter = if config.ratelimit > 0 {
            Some(Arc::new(Semaphore::new(config.ratelimit as usize)))
        } else {
            None
        };

        let cache = DnsCache::new();

        if config.cache
            && let Some(path) = &config.cache_file
            && let Err(e) = cache.load_from_disk(path, config.max_cache_size).await
        {
            warn!("Failed to load cache from {}: {}", path.display(), e);
        }

        Ok(Self {
            config,
            system_resolver,
            cache,
            round_robin_index: Arc::new(RwLock::new(0)),
            rate_limiter,
            connection_pool: ConnectionPool::new(),
        })
    }

    pub async fn handle_query(&self, query_msg: Message, _src: SocketAddr) -> Message {
        // Enforce concurrency rate limit
        let _permit = if let Some(limiter) = &self.rate_limiter {
            match limiter.try_acquire() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    return create_error_response(query_msg.metadata.id, ResponseCode::Refused);
                }
            }
        } else {
            None
        };

        if query_msg.queries.is_empty() {
            return create_error_response(query_msg.metadata.id, ResponseCode::FormErr);
        }

        let query = &query_msg.queries[0];
        let name = query.name.clone();
        let record_type = query.query_type;

        // Check cache
        if self.config.cache
            && let Some(mut cached_msg) = self.cache.get(&name, record_type).await
        {
            cached_msg.metadata.id = query_msg.metadata.id;
            debug!("Cache hit for {:?} {:?}", name, record_type);
            cached_msg = self.filter_ipv6(cached_msg, &name).await;
            if !cached_msg.answers.is_empty() {
                return cached_msg;
            }
        }

        // Forward to upstream
        let response = self.forward_to_upstream(query_msg.clone()).await;

        // Filter IPv6 if configured
        let response = self.filter_ipv6(response, &name).await;

        // Cache response on success
        if self.config.cache && response.metadata.response_code == ResponseCode::NoError {
            let ttl = response
                .answers
                .iter()
                .map(|r| r.ttl)
                .min()
                .unwrap_or(300);

            if ttl > 0 {
                let cache_ttl = std::cmp::min(
                    Duration::from_secs(ttl as u64),
                    self.config.max_cache_ttl,
                );
                let expiry = SystemTime::now() + cache_ttl;
                self.cache.insert(name, record_type, response.clone(), expiry).await;

                if let Err(e) = self.cache.prune_and_limit(self.config.max_cache_size).await {
                    warn!("Cache maintenance failed: {}", e);
                }
                if let Some(path) = &self.config.cache_file
                    && let Err(e) = self.cache.save_to_disk(path).await
                {
                    warn!("Failed to save cache to {}: {}", path.display(), e);
                }
            }
        }

        response
    }

    pub async fn filter_ipv6(&self, message: Message, query_name: &Name) -> Message {
        if !self.config.ipv6_disabled {
            return message;
        }

        let domain = query_name.to_utf8();
        for pattern in &self.config.ipv6_whitelist {
            if matches_wildcard(&domain, pattern) {
                return message;
            }
        }
        info!("IPv6 answers filtered for domain: {}", domain);

        // Rebuild message without AAAA records
        let mut new_message = Message::new(
            message.metadata.id,
            message.metadata.message_type,
            message.metadata.op_code,
        );
        new_message.metadata.response_code = message.metadata.response_code;
        new_message.metadata.authoritative = message.metadata.authoritative;
        new_message.metadata.truncation = message.metadata.truncation;
        new_message.metadata.recursion_desired = message.metadata.recursion_desired;
        new_message.metadata.recursion_available = message.metadata.recursion_available;
        new_message.metadata.authentic_data = message.metadata.authentic_data;
        new_message.metadata.checking_disabled = message.metadata.checking_disabled;

        for query in &message.queries {
            new_message.add_query(query.clone());
        }

        for answer in &message.answers {
            if answer.record_type() != RecordType::AAAA {
                new_message.add_answer(answer.clone());
            }
        }

        for add in &message.additionals {
            new_message.add_additional(add.clone());
        }

        new_message
    }

    pub async fn forward_to_upstream(&self, query: Message) -> Message {
        match self.config.upstream_mode {
            UpstreamMode::Parallel => self.forward_parallel(query).await,
            UpstreamMode::LoadBalance | UpstreamMode::RoundRobin => {
                self.forward_round_robin(query).await
            }
        }
    }

    pub async fn forward_parallel(&self, query: Message) -> Message {
        let mut tasks = Vec::new();

        for upstream in &self.config.upstreams {
            let upstream = upstream.clone();
            let query = query.clone();
            let resolver = self.system_resolver.clone();
            let timeout_duration = self.config.timeout;
            let pool = self.connection_pool.clone();

            let task = tokio::spawn(async move {
                match timeout(
                    timeout_duration,
                    query_upstream(&upstream, query, resolver, &pool),
                )
                .await
                {
                    Ok(Ok(response)) => Some(response),
                    Ok(Err(e)) => {
                        debug!("Upstream {:?} failed: {}", upstream.url(), e);
                        None
                    }
                    Err(_) => {
                        debug!("Upstream {:?} timed out", upstream.url());
                        None
                    }
                }
            });

            tasks.push(task);
        }

        // Return first successful upstream response
        while !tasks.is_empty() {
            let (result, _index, remaining) = futures_util::future::select_all(tasks).await;
            if let Ok(Some(response)) = result {
                return response;
            }
            tasks = remaining;
        }

        create_error_response(query.metadata.id, ResponseCode::ServFail)
    }

    pub async fn forward_round_robin(&self, query: Message) -> Message {
        let upstreams_count = self.config.upstreams.len();
        if upstreams_count == 0 {
            return create_error_response(query.metadata.id, ResponseCode::ServFail);
        }

        let start_index = {
            let mut index = self.round_robin_index.write().await;
            let current = *index;
            *index = (*index + 1) % upstreams_count;
            current
        };

        for i in 0..upstreams_count {
            let upstream_index = (start_index + i) % upstreams_count;
            let upstream = &self.config.upstreams[upstream_index];

            match timeout(
                self.config.timeout,
                query_upstream(
                    upstream,
                    query.clone(),
                    self.system_resolver.clone(),
                    &self.connection_pool,
                ),
            )
            .await
            {
                Ok(Ok(response)) => return response,
                Ok(Err(e)) => {
                    debug!("Upstream {:?} failed: {}", upstream.url(), e);
                }
                Err(_) => {
                    debug!("Upstream {:?} timed out", upstream.url());
                }
            }
        }

        create_error_response(query.metadata.id, ResponseCode::ServFail)
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut handles = Vec::new();

        for addr in &self.config.listen_addrs {
            for port in &self.config.listen_ports {
                let bind_addr = SocketAddr::new(*addr, *port);

                // UDP listener
                let udp_socket = Arc::new(UdpSocket::bind(bind_addr).await?);
                info!("Listening on UDP {}", bind_addr);

                let proxy = self.clone();
                let udp_handle = tokio::spawn(async move {
                    proxy.run_udp(udp_socket).await;
                });
                handles.push(udp_handle);

                // TCP listener
                let tcp_listener = TcpListener::bind(bind_addr).await?;
                info!("Listening on TCP {}", bind_addr);

                let proxy = self.clone();
                let tcp_handle = tokio::spawn(async move {
                    proxy.run_tcp(tcp_listener).await;
                });
                handles.push(tcp_handle);
            }
        }

        futures_util::future::join_all(handles).await;
        Ok(())
    }

    async fn run_udp(&self, socket: Arc<UdpSocket>) {
        let mut buf = vec![0u8; 4096];

        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    let data = buf[..len].to_vec();
                    let socket = socket.clone();
                    let proxy = self.clone();

                    tokio::spawn(async move {
                        match Message::from_bytes(&data) {
                            Ok(query) => {
                                let response = proxy.handle_query(query, src).await;
                                if let Ok(response_bytes) = response.to_bytes() {
                                    let _ = socket.send_to(&response_bytes, src).await;
                                }
                            }
                            Err(e) => {
                                debug!("Failed to parse UDP DNS query from {}: {}", src, e);
                            }
                        }
                    });
                }
                Err(e) => {
                    error!("UDP receive error: {}", e);
                }
            }
        }
    }

    async fn run_tcp(&self, listener: TcpListener) {
        loop {
            match listener.accept().await {
                Ok((stream, src)) => {
                    let proxy = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = proxy.handle_tcp_connection(stream, src).await {
                            debug!("TCP connection error from {}: {}", src, e);
                        }
                    });
                }
                Err(e) => {
                    error!("TCP accept error: {}", e);
                }
            }
        }
    }

    async fn handle_tcp_connection(
        &self,
        mut stream: TcpStream,
        src: SocketAddr,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        loop {
            // Read length prefix (2 bytes) per RFC 1035 / 7766
            let mut len_buf = [0u8; 2];
            match stream.read_exact(&mut len_buf).await {
                Ok(_) => {}
                Err(_) => return Ok(()), // Connection closed cleanly by client
            }

            let msg_len = u16::from_be_bytes(len_buf) as usize;
            if msg_len == 0 || msg_len > 65535 {
                return Ok(());
            }

            let mut msg_buf = vec![0u8; msg_len];
            stream.read_exact(&mut msg_buf).await?;

            let query = Message::from_bytes(&msg_buf)?;
            let response = self.handle_query(query, src).await;
            let response_bytes = response.to_bytes()?;

            let len = (response_bytes.len() as u16).to_be_bytes();
            stream.write_all(&len).await?;
            stream.write_all(&response_bytes).await?;
        }
    }
}

pub fn matches_wildcard(domain: &str, pattern: &str) -> bool {
    let domain = domain.trim_end_matches('.').to_lowercase();
    let pattern = pattern.trim_end_matches('.').to_lowercase();

    if let Some(suffix) = pattern.strip_prefix("*.") {
        (domain.ends_with(&suffix)
            && (domain.len() == suffix.len()
                || domain.as_bytes().get(domain.len() - suffix.len() - 1) == Some(&b'.')))
            || domain == suffix
    } else {
        domain == pattern
    }
}

pub fn create_error_response(id: u16, code: ResponseCode) -> Message {
    let mut response = Message::new(id, MessageType::Response, OpCode::Query);
    response.metadata.response_code = code;
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_wildcard() {
        assert!(matches_wildcard("example.com", "example.com"));
        assert!(matches_wildcard("example.com.", "example.com"));
        assert!(matches_wildcard("example.com", "example.com."));
        assert!(matches_wildcard("EXAMPLE.COM.", "example.com"));

        assert!(matches_wildcard("example.com", "*.example.com"));
        assert!(matches_wildcard("sub.example.com", "*.example.com"));
        assert!(matches_wildcard("sub.example.com.", "*.example.com"));
        assert!(matches_wildcard("deep.sub.example.com", "*.example.com"));

        assert!(!matches_wildcard("fakeexample.com", "*.example.com"));
        assert!(!matches_wildcard("other.org", "example.com"));
        assert!(!matches_wildcard("other.org", "*.example.com"));
    }

    #[test]
    fn test_create_error_response() {
        let msg = create_error_response(42, ResponseCode::ServFail);
        assert_eq!(msg.metadata.id, 42);
        assert_eq!(msg.metadata.response_code, ResponseCode::ServFail);
        assert_eq!(msg.metadata.message_type, MessageType::Response);
    }
}
