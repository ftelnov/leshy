use hickory_proto::op::Message;
use hickory_proto::rr::RecordType;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct DnsCache {
    entries: Mutex<HashMap<CacheKey, CacheEntry>>,
    max_entries: usize,
}

#[derive(Hash, Eq, PartialEq)]
struct CacheKey {
    qname: String,
    qtype: RecordType,
}

struct CacheEntry {
    message: Message,
    inserted_at: Instant,
    ttl: Duration,
}

impl DnsCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_entries,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.max_entries > 0
    }

    pub fn lookup(&self, qname: &str, qtype: RecordType) -> Option<Message> {
        let key = CacheKey {
            qname: qname.to_lowercase(),
            qtype,
        };
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get(&key) {
            if entry.inserted_at.elapsed() < entry.ttl {
                return Some(entry.message.clone());
            }
            entries.remove(&key);
        }
        None
    }

    pub fn insert(&self, qname: &str, qtype: RecordType, message: Message, ttl: Duration) {
        if !self.is_enabled() {
            return;
        }
        let key = CacheKey {
            qname: qname.to_lowercase(),
            qtype,
        };
        let mut entries = self.entries.lock().unwrap();

        // If at capacity and this is a new key, sweep expired entries
        if entries.len() >= self.max_entries && !entries.contains_key(&key) {
            entries.retain(|_, entry| entry.inserted_at.elapsed() < entry.ttl);
        }

        // If still at capacity after sweep, skip insertion
        if entries.len() >= self.max_entries && !entries.contains_key(&key) {
            return;
        }

        entries.insert(
            key,
            CacheEntry {
                message,
                inserted_at: Instant::now(),
                ttl,
            },
        );
    }

    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::op::{MessageType, ResponseCode};
    use hickory_proto::rr::{Name, RData, Record};
    use std::net::Ipv4Addr;
    use std::str::FromStr;

    fn make_response(name: &str, ip: Ipv4Addr, ttl: u32) -> Message {
        let mut msg = Message::new();
        msg.set_message_type(MessageType::Response);
        msg.set_response_code(ResponseCode::NoError);
        let mut record = Record::from_rdata(
            Name::from_str(name).unwrap(),
            ttl,
            RData::A(hickory_proto::rr::rdata::A(ip)),
        );
        record.set_record_type(RecordType::A);
        msg.add_answer(record);
        msg
    }

    #[test]
    fn test_disabled_cache() {
        let cache = DnsCache::new(0);
        assert!(!cache.is_enabled());
        cache.insert(
            "example.com",
            RecordType::A,
            Message::new(),
            Duration::from_secs(60),
        );
        assert!(cache.lookup("example.com", RecordType::A).is_none());
    }

    #[test]
    fn test_insert_and_lookup() {
        let cache = DnsCache::new(100);
        let msg = make_response("example.com.", Ipv4Addr::new(1, 2, 3, 4), 300);

        cache.insert(
            "example.com.",
            RecordType::A,
            msg.clone(),
            Duration::from_secs(60),
        );

        let cached = cache.lookup("example.com.", RecordType::A);
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().answers().len(), 1);
    }

    #[test]
    fn test_case_insensitive() {
        let cache = DnsCache::new(100);
        let msg = make_response("Example.COM.", Ipv4Addr::new(1, 2, 3, 4), 300);

        cache.insert("Example.COM.", RecordType::A, msg, Duration::from_secs(60));
        assert!(cache.lookup("example.com.", RecordType::A).is_some());
    }

    #[test]
    fn test_expired_entry_removed() {
        let cache = DnsCache::new(100);
        let msg = make_response("example.com.", Ipv4Addr::new(1, 2, 3, 4), 300);

        cache.insert("example.com.", RecordType::A, msg, Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));

        assert!(cache.lookup("example.com.", RecordType::A).is_none());
    }

    #[test]
    fn test_different_qtypes() {
        let cache = DnsCache::new(100);
        let msg = make_response("example.com.", Ipv4Addr::new(1, 2, 3, 4), 300);

        cache.insert("example.com.", RecordType::A, msg, Duration::from_secs(60));
        assert!(cache.lookup("example.com.", RecordType::A).is_some());
        assert!(cache.lookup("example.com.", RecordType::AAAA).is_none());
    }

    #[test]
    fn test_clear() {
        let cache = DnsCache::new(100);
        let msg = make_response("example.com.", Ipv4Addr::new(1, 2, 3, 4), 300);

        cache.insert("example.com.", RecordType::A, msg, Duration::from_secs(60));
        cache.clear();
        assert!(cache.lookup("example.com.", RecordType::A).is_none());
    }

    #[test]
    fn test_capacity_sweep() {
        let cache = DnsCache::new(2);
        let msg1 = make_response("a.com.", Ipv4Addr::new(1, 1, 1, 1), 300);
        let msg2 = make_response("b.com.", Ipv4Addr::new(2, 2, 2, 2), 300);
        let msg3 = make_response("c.com.", Ipv4Addr::new(3, 3, 3, 3), 300);

        // Insert with very short TTL so they expire
        cache.insert("a.com.", RecordType::A, msg1, Duration::from_millis(1));
        cache.insert("b.com.", RecordType::A, msg2, Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));

        // This should trigger sweep of expired entries and succeed
        cache.insert("c.com.", RecordType::A, msg3, Duration::from_secs(60));
        assert!(cache.lookup("c.com.", RecordType::A).is_some());
    }
}
