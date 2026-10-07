use crate::config::Upstream;
use crate::pool::ConnectionPool;
use hickory_proto::op::Message;
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};
use hickory_resolver::TokioResolver;
use std::net::{IpAddr, SocketAddr};
use tokio::net::UdpSocket;
use tracing::debug;

pub async fn query_upstream(
    upstream: &Upstream,
    query: Message,
    resolver: Option<TokioResolver>,
    pool: &ConnectionPool,
) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
    let url = upstream.url();

    // Support https://, h3://, tls://, quic://, and plain UDP DNS
    if url.starts_with("https://") || url.starts_with("h3://") {
        query_https(upstream, query, pool).await
    } else if url.starts_with("tls://") {
        debug!("TLS upstream is not currently supported, falling back");
        Err("TLS upstream not implemented".into())
    } else if url.starts_with("quic://") {
        debug!("QUIC upstream is not currently supported, falling back");
        Err("QUIC upstream not implemented".into())
    } else {
        query_plain(url, query, resolver).await
    }
}

pub async fn query_https(
    upstream: &Upstream,
    query: Message,
    pool: &ConnectionPool,
) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
    let url = upstream.url().replace("h3://", "https://");
    let query_bytes = query.to_bytes()?;

    let client = pool.get_client(upstream).await?;

    let response = client
        .post(&url)
        .header("content-type", "application/dns-message")
        .header("accept", "application/dns-message")
        .body(query_bytes)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("Upstream DoH HTTP status: {}", response.status()).into());
    }

    let response_bytes = response.bytes().await?;
    let message = Message::from_bytes(&response_bytes)?;

    Ok(message)
}

pub async fn query_plain(
    url: &str,
    query: Message,
    resolver: Option<TokioResolver>,
) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
    let (host, port) = parse_host_port(url, 53)?;

    let ip = resolve_host(&host, resolver).await?;
    let socket_addr = SocketAddr::new(ip, port);

    let bind_addr: SocketAddr = if ip.is_ipv4() {
        "0.0.0.0:0".parse()?
    } else {
        "[::]:0".parse()?
    };

    let socket = UdpSocket::bind(bind_addr).await?;
    socket.connect(socket_addr).await?;

    let query_bytes = query.to_bytes()?;
    socket.send(&query_bytes).await?;

    let mut buf = vec![0u8; 4096];
    let len = socket.recv(&mut buf).await?;

    let response = Message::from_bytes(&buf[..len])?;
    Ok(response)
}

pub async fn resolve_host(
    host: &str,
    resolver: Option<TokioResolver>,
) -> Result<IpAddr, Box<dyn std::error::Error + Send + Sync>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }

    let host = host.trim_matches(|c| c == '[' || c == ']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }

    if let Some(resolver) = resolver {
        let response = resolver.lookup_ip(host).await?;
        if let Some(ip) = response.iter().next() {
            return Ok(ip);
        }
    }

    Err(format!("Failed to resolve upstream host: {}", host).into())
}

pub fn parse_host_port(
    s: &str,
    default_port: u16,
) -> Result<(String, u16), Box<dyn std::error::Error + Send + Sync>> {
    let s = s.trim();

    // Strip scheme if present (e.g. plain://, udp://, dns://)
    let s = if let Some(stripped) = s.strip_prefix("plain://") {
        stripped
    } else if let Some(stripped) = s.strip_prefix("udp://") {
        stripped
    } else if let Some(stripped) = s.strip_prefix("dns://") {
        stripped
    } else {
        s
    };

    // Handle IPv6 in brackets: [2001:db8::1]:53 or [2001:db8::1]
    if s.starts_with('[')
        && let Some(bracket_end) = s.find(']')
    {
        let host = s[1..bracket_end].to_string();
        let port = if s.len() > bracket_end + 1 && s.as_bytes()[bracket_end + 1] == b':' {
            s[bracket_end + 2..].parse()?
        } else {
            default_port
        };
        return Ok((host, port));
    }

    // Handle regular host:port or just host
    if let Some(colon_pos) = s.rfind(':')
        && let Ok(port) = s[colon_pos + 1..].parse::<u16>()
    {
        let host = s[..colon_pos].to_string();
        return Ok((host, port));
    }

    Ok((s.to_string(), default_port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_host_port() {
        assert_eq!(
            parse_host_port("8.8.8.8", 53).unwrap(),
            ("8.8.8.8".to_string(), 53)
        );
        assert_eq!(
            parse_host_port("8.8.8.8:5353", 53).unwrap(),
            ("8.8.8.8".to_string(), 5353)
        );
        assert_eq!(
            parse_host_port("udp://8.8.8.8:5353", 53).unwrap(),
            ("8.8.8.8".to_string(), 5353)
        );
        assert_eq!(
            parse_host_port("plain://1.1.1.1", 53).unwrap(),
            ("1.1.1.1".to_string(), 53)
        );
        assert_eq!(
            parse_host_port("[2001:4860:4860::8888]", 53).unwrap(),
            ("2001:4860:4860::8888".to_string(), 53)
        );
        assert_eq!(
            parse_host_port("[2001:4860:4860::8888]:5353", 53).unwrap(),
            ("2001:4860:4860::8888".to_string(), 5353)
        );
        assert_eq!(
            parse_host_port("dns.google", 53).unwrap(),
            ("dns.google".to_string(), 53)
        );
    }
}
