use serde::{Deserialize, Deserializer, Serialize};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::info;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    #[serde(default = "default_log_level")]
    pub log_level: String,

    #[serde(default)]
    pub bootstrap: Vec<String>,

    #[serde(default = "default_listen_addrs")]
    pub listen_addrs: Vec<IpAddr>,

    #[serde(default = "default_listen_ports")]
    pub listen_ports: Vec<u16>,

    #[serde(default)]
    pub upstream_mode: UpstreamMode,

    #[serde(default = "default_timeout", deserialize_with = "deserialize_duration")]
    pub timeout: Duration,

    pub upstreams: Vec<Upstream>,

    #[serde(default)]
    pub cache: bool,

    #[serde(default)]
    pub cache_file: Option<PathBuf>,

    #[serde(default = "default_max_cache_size", deserialize_with = "deserialize_size")]
    pub max_cache_size: u64,

    #[serde(default = "default_max_cache_ttl", deserialize_with = "deserialize_duration")]
    pub max_cache_ttl: Duration,

    #[serde(default)]
    pub ratelimit: u32,

    #[serde(default)]
    pub ipv6_disabled: bool,

    #[serde(default)]
    pub ipv6_whitelist: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamMode {
    #[default]
    Parallel,
    LoadBalance,
    #[serde(alias = "round_robin")]
    RoundRobin,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Upstream {
    Simple(String),
    WithProxy {
        url: String,
        socks5: String,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        password: Option<String>,
    },
}

impl Upstream {
    pub fn url(&self) -> &str {
        match self {
            Upstream::Simple(url) => url,
            Upstream::WithProxy { url, .. } => url,
        }
    }

    pub fn socks5_config(&self) -> Option<(&str, Option<&str>, Option<&str>)> {
        match self {
            Upstream::Simple(_) => None,
            Upstream::WithProxy {
                socks5,
                username,
                password,
                ..
            } => Some((socks5.as_str(), username.as_deref(), password.as_deref())),
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_listen_addrs() -> Vec<IpAddr> {
    vec!["127.0.0.1".parse().expect("valid fallback ip")]
}

fn default_listen_ports() -> Vec<u16> {
    vec![53]
}

fn default_timeout() -> Duration {
    Duration::from_secs(10)
}

fn default_max_cache_size() -> u64 {
    50 * 1024 * 1024 // 50 MiB
}

fn default_max_cache_ttl() -> Duration {
    Duration::from_secs(86400) // 1 day
}

fn deserialize_duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_duration(&s).map_err(serde::de::Error::custom)
}

pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("Duration string cannot be empty".to_string());
    }

    let mut total_millis = 0u64;
    let mut current_num = String::new();
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch.is_ascii_digit() {
            current_num.push(ch);
        } else {
            if current_num.is_empty() {
                return Err(format!("Invalid duration format: {}", s));
            }

            let num: u64 = current_num
                .parse()
                .map_err(|_| format!("Invalid number in duration: {}", current_num))?;
            current_num.clear();

            match ch {
                'd' => total_millis += num * 86_400_000,
                'h' => total_millis += num * 3_600_000,
                'm' => {
                    if chars.peek() == Some(&'s') {
                        chars.next();
                        total_millis += num;
                    } else {
                        total_millis += num * 60_000;
                    }
                }
                's' => total_millis += num * 1000,
                _ => return Err(format!("Invalid duration unit: {}", ch)),
            }
        }
    }

    if !current_num.is_empty() {
        return Err(format!("Duration missing unit: {}", s));
    }

    Ok(Duration::from_millis(total_millis))
}

fn deserialize_size<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_size(&s).map_err(serde::de::Error::custom)
}

pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let split_idx = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let (num_part, unit_part) = s.split_at(split_idx);

    let num: f64 = num_part
        .trim()
        .parse()
        .map_err(|_| format!("Invalid size number: '{}'", num_part))?;

    let multiplier: u64 = match unit_part.trim().to_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        _ => return Err(format!("Invalid size unit: '{}'", unit_part)),
    };

    Ok((num * multiplier as f64) as u64)
}

impl Config {
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let content = std::fs::read_to_string(path.as_ref())?;
        info!("Loading configuration from {}", path.as_ref().display());
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86400));
        assert_eq!(
            parse_duration("1d1h1m1s").unwrap(),
            Duration::from_secs(86400 + 3600 + 60 + 1)
        );
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert!(parse_duration("invalid").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1024B").unwrap(), 1024);
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("10KB").unwrap(), 10 * 1024);
        assert_eq!(parse_size("50M").unwrap(), 50 * 1024 * 1024);
        assert_eq!(parse_size("50MB").unwrap(), 50 * 1024 * 1024);
        assert_eq!(parse_size("50MiB").unwrap(), 50 * 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("1GB").unwrap(), 1024 * 1024 * 1024);
        assert!(parse_size("abc").is_err());
    }

    #[test]
    fn test_deserialize_config() {
        let toml_str = r#"
            listen_addrs = ["127.0.0.1"]
            listen_ports = [5300]
            upstream_mode = "round_robin"
            timeout = "5s"
            upstreams = [
                "https://1.1.1.1/dns-query",
                { url = "https://8.8.8.8/dns-query", socks5 = "socks5://127.0.0.1:1080", username = "u", password = "p" }
            ]
            cache = true
            max_cache_size = "100M"
            max_cache_ttl = "2h"
            ratelimit = 100
            ipv6_disabled = true
            ipv6_whitelist = ["*.example.com"]
        "#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.listen_ports, vec![5300]);
        assert_eq!(config.upstream_mode, UpstreamMode::RoundRobin);
        assert_eq!(config.timeout, Duration::from_secs(5));
        assert_eq!(config.upstreams.len(), 2);
        assert_eq!(config.max_cache_size, 100 * 1024 * 1024);
        assert_eq!(config.max_cache_ttl, Duration::from_secs(7200));
        assert_eq!(config.ratelimit, 100);
        assert!(config.ipv6_disabled);
        assert_eq!(config.ipv6_whitelist, vec!["*.example.com"]);
    }
}
