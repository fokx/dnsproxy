use hickory_proto::op::Message;
use hickory_proto::rr::{Name, RecordType};
use hickory_proto::serialize::binary::{BinDecodable, BinEncodable};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::RwLock;
use tracing::warn;

#[derive(Clone)]
pub struct CacheEntry {
    pub message: Message,
    pub expiry: SystemTime,
}

#[derive(Serialize, Deserialize)]
struct DiskCacheEntry {
    name: String,
    record_type: u16,
    expiry_ts: u64,
    message: Vec<u8>,
}

#[derive(Serialize, Deserialize, Default)]
struct DiskCache {
    entries: Vec<DiskCacheEntry>,
}

#[derive(Clone, Default)]
pub struct DnsCache {
    inner: Arc<RwLock<HashMap<(Name, RecordType), CacheEntry>>>,
}

impl DnsCache {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get(&self, name: &Name, record_type: RecordType) -> Option<Message> {
        let cache = self.inner.read().await;
        if let Some(entry) = cache.get(&(name.clone(), record_type))
            && SystemTime::now() < entry.expiry
        {
            return Some(entry.message.clone());
        }
        None
    }

    pub async fn insert(&self, name: Name, record_type: RecordType, message: Message, expiry: SystemTime) {
        let mut cache = self.inner.write().await;
        cache.insert((name, record_type), CacheEntry { message, expiry });
    }

    pub async fn prune_and_limit(
        &self,
        max_size: u64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut cache = self.inner.write().await;

        // 1) Remove expired entries
        let now = SystemTime::now();
        cache.retain(|_, entry| entry.expiry > now);

        // 2) Enforce size limit
        let mut size: u64 = 0;
        let mut sizes: Vec<((Name, RecordType), u64, SystemTime)> = Vec::with_capacity(cache.len());

        for (k, v) in cache.iter() {
            let (name, rtype) = k;
            let msg_size = v.message.to_bytes().map(|b| b.len() as u64).unwrap_or(0);
            let key_overhead = (name.to_string().len() as u64) + 8;
            let entry_size = msg_size + key_overhead + 16;
            sizes.push(((name.clone(), *rtype), entry_size, v.expiry));
            size = size.saturating_add(entry_size);
        }

        if size > max_size {
            // Sort by expiry ascending (earliest to expire removed first)
            sizes.sort_by_key(|(_k, _s, exp)| *exp);
            for ((name, rtype), entry_size, _exp) in sizes {
                if size <= max_size {
                    break;
                }
                if cache.remove(&(name, rtype)).is_some() {
                    size = size.saturating_sub(entry_size);
                }
            }
        }

        Ok(())
    }

    pub async fn load_from_disk(
        &self,
        path: &Path,
        max_size: u64,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !path.exists() {
            return Ok(());
        }

        let mut file = fs::File::open(path)?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        let disk: DiskCache = toml::from_str(&buf)?;

        let now = SystemTime::now();
        let mut map: HashMap<(Name, RecordType), CacheEntry> = HashMap::new();
        let mut total_size: u64 = 0;

        for e in disk.entries.into_iter() {
            let expiry = SystemTime::UNIX_EPOCH + Duration::from_secs(e.expiry_ts);
            if expiry <= now {
                continue;
            }

            let name = match Name::from_str(&e.name) {
                Ok(n) => n,
                Err(err) => {
                    warn!("Skipping invalid domain in disk cache entry '{}': {}", e.name, err);
                    continue;
                }
            };
            let rtype = RecordType::from(e.record_type);

            match Message::from_bytes(&e.message) {
                Ok(message) => {
                    let key_overhead = (e.name.len() as u64) + 8 + 16;
                    total_size = total_size.saturating_add(e.message.len() as u64 + key_overhead);
                    map.insert((name, rtype), CacheEntry { message, expiry });
                }
                Err(err) => {
                    warn!("Skipping corrupt cache entry {} {:?}: {}", e.name, e.record_type, err);
                }
            }

            if total_size > max_size {
                break;
            }
        }

        let mut cache = self.inner.write().await;
        *cache = map;
        Ok(())
    }

    pub async fn save_to_disk(&self, path: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let cache = self.inner.read().await;
        let mut disk = DiskCache {
            entries: Vec::with_capacity(cache.len()),
        };

        let now = SystemTime::now();
        for ((name, rtype), entry) in cache.iter() {
            if entry.expiry <= now {
                continue;
            }

            let message_bytes = match entry.message.to_bytes() {
                Ok(b) => b,
                Err(_) => continue,
            };

            let expiry_ts = entry
                .expiry
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            disk.entries.push(DiskCacheEntry {
                name: name.to_string(),
                record_type: (*rtype).into(),
                expiry_ts,
                message: message_bytes,
            });
        }

        let data = toml::to_string(&disk)?;
        let tmp_path = path.with_extension("tmp");
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
        fs::rename(tmp_path, path)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::op::{MessageType, OpCode};

    #[tokio::test]
    async fn test_cache_insert_and_get() {
        let cache = DnsCache::new();
        let name = Name::from_str("example.com.").unwrap();
        let rtype = RecordType::A;

        let msg = Message::new(1234, MessageType::Response, OpCode::Query);
        let expiry = SystemTime::now() + Duration::from_secs(60);

        cache.insert(name.clone(), rtype, msg.clone(), expiry).await;

        let retrieved = cache.get(&name, rtype).await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().metadata.id, 1234);
    }

    #[tokio::test]
    async fn test_cache_expired_returns_none() {
        let cache = DnsCache::new();
        let name = Name::from_str("expired.com.").unwrap();
        let rtype = RecordType::A;

        let msg = Message::new(5678, MessageType::Response, OpCode::Query);
        let expiry = SystemTime::now() - Duration::from_secs(1);

        cache.insert(name.clone(), rtype, msg, expiry).await;

        let retrieved = cache.get(&name, rtype).await;
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn test_disk_cache_persistence() {
        let temp_dir = std::env::temp_dir();
        let cache_file = temp_dir.join(format!("dnsproxy_test_cache_{}.toml", std::process::id()));

        let cache = DnsCache::new();
        let name = Name::from_str("persisted.org.").unwrap();
        let rtype = RecordType::A;
        let msg = Message::new(9999, MessageType::Response, OpCode::Query);
        let expiry = SystemTime::now() + Duration::from_secs(3600);

        cache.insert(name.clone(), rtype, msg, expiry).await;
        cache.save_to_disk(&cache_file).await.unwrap();

        let new_cache = DnsCache::new();
        new_cache.load_from_disk(&cache_file, 1024 * 1024).await.unwrap();

        let loaded = new_cache.get(&name, rtype).await;
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().metadata.id, 9999);

        let _ = fs::remove_file(cache_file);
    }
}
