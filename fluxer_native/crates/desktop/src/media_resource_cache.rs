// SPDX-License-Identifier: AGPL-3.0-or-later
//! Session-local encoded media reuse, independent of decoder/GPU lifetimes.
use reqwest::header::{self, HeaderMap};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

const MAX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 128;
const IDLE: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
pub(super) enum Body {
    Memory(Arc<[u8]>),
    Range(Arc<CachedRange>),
    // Unnamed disk files disappear when their final descriptor closes, including
    // process termination. No signed URLs or stale persistent cache index.
    Disk(Arc<Mutex<File>>, u64),
}
pub(super) struct CachedRange {
    pub bytes: Arc<[u8]>,
    pub total: u64,
    pub validator: (header::HeaderName, header::HeaderValue),
}
impl Body {
    pub fn len(&self) -> u64 {
        match self {
            Self::Memory(bytes) => bytes.len() as u64,
            Self::Range(range) => range.bytes.len() as u64,
            Self::Disk(_, length) => *length,
        }
    }
    pub fn read_at(&self, position: u64, out: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Memory(bytes) => {
                let start = position.min(bytes.len() as u64) as usize;
                let count = out.len().min(bytes.len() - start);
                out[..count].copy_from_slice(&bytes[start..start + count]);
                Ok(count)
            }
            Self::Range(range) => {
                let start = position.min(range.bytes.len() as u64) as usize;
                let count = out.len().min(range.bytes.len() - start);
                out[..count].copy_from_slice(&range.bytes[start..start + count]);
                Ok(count)
            }
            Self::Disk(file, _) => {
                let mut file = file.lock().unwrap_or_else(|error| error.into_inner());
                file.seek(SeekFrom::Start(position))?;
                file.read(out)
            }
        }
    }
}

struct Entry {
    body: Body,
    used: Instant,
    expires: Instant,
}
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Complete(String),
    Range(String, u64),
}
struct Cache {
    entries: HashMap<Key, Entry>,
    bytes: u64,
    limit: u64,
}
impl Cache {
    fn new(limit: u64) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            limit,
        }
    }
    fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, entry| {
            let keep = now < entry.expires && now.duration_since(entry.used) < IDLE;
            if !keep {
                self.bytes -= entry.body.len();
            }
            keep
        });
    }
    fn invalidate(&mut self, url: &str) {
        self.entries.retain(|key, entry| {
            let (Key::Complete(entry_url) | Key::Range(entry_url, _)) = key;
            if entry_url == url {
                self.bytes -= entry.body.len();
                false
            } else {
                true
            }
        });
    }
    fn get(&mut self, url: &str, now: Instant) -> Option<Body> {
        self.get_key(&Key::Complete(url.to_owned()), now)
    }
    fn get_key(&mut self, key: &Key, now: Instant) -> Option<Body> {
        self.prune(now);
        let entry = self.entries.get_mut(key)?;
        entry.used = now;
        Some(entry.body.clone())
    }
    fn insert(&mut self, url: String, body: Body, expires: Instant, now: Instant) {
        self.insert_key(Key::Complete(url), body, expires, now);
    }
    fn insert_key(&mut self, key: Key, body: Body, expires: Instant, now: Instant) {
        self.prune(now);
        if body.len() == 0 || body.len() > self.limit || expires <= now {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.bytes -= old.body.len();
        }
        while self.bytes + body.len() > self.limit || self.entries.len() >= MAX_ENTRIES {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.bytes -= self.entries.remove(&oldest).unwrap().body.len();
        }
        self.bytes += body.len();
        self.entries.insert(
            key,
            Entry {
                body,
                used: now,
                expires,
            },
        );
    }
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::new(MAX_BYTES)))
}
pub(super) fn get(url: &str) -> Option<Body> {
    cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(url, Instant::now())
}
pub(super) fn invalidate(url: &str) {
    cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .invalidate(url);
}
pub(super) fn insert(url: &str, body: Body, expires: Option<Instant>) {
    if let Some(expires) = expires {
        cache()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(url.to_owned(), body, expires, Instant::now());
    }
}
pub(super) fn get_range(url: &str, start: u64) -> Option<Arc<CachedRange>> {
    let Body::Range(range) = cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get_key(&Key::Range(url.to_owned(), start), Instant::now())?
    else {
        return None;
    };
    Some(range)
}
pub(super) fn insert_range(url: &str, start: u64, range: CachedRange, expires: Option<Instant>) {
    if let Some(expires) = expires {
        cache()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert_key(
                Key::Range(url.to_owned(), start),
                Body::Range(Arc::new(range)),
                expires,
                Instant::now(),
            );
    }
}

// Reuse only explicitly fresh responses. No-store/no-cache and already-aged
// responses require a new fetch; the original signed URL is the entire key.
pub(super) fn expiration(headers: &HeaderMap) -> Option<Instant> {
    let mut max_age = None;
    for value in headers.get_all(header::CACHE_CONTROL) {
        for directive in value.to_str().ok()?.split(',') {
            let (name, value) = directive
                .trim()
                .split_once('=')
                .unwrap_or((directive.trim(), ""));
            if name.eq_ignore_ascii_case("no-store") || name.eq_ignore_ascii_case("no-cache") {
                return None;
            }
            if name.eq_ignore_ascii_case("max-age") {
                let age = value.trim().trim_matches('"').parse::<u64>().ok()?;
                max_age = Some(max_age.map_or(age, |previous: u64| previous.min(age)));
            }
        }
    }
    if headers.get_all(header::VARY).iter().any(|value| {
        value
            .to_str()
            .ok()
            .is_none_or(|value| value.split(',').any(|part| part.trim() == "*"))
    }) {
        return None;
    }
    let age = headers
        .get(header::AGE)
        .map(|value| value.to_str().ok()?.parse::<u64>().ok())
        .unwrap_or(Some(0))?;
    let ttl = max_age?.saturating_sub(age);
    if ttl == 0 {
        return None;
    }
    Instant::now().checked_add(Duration::from_secs(ttl))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn body(length: usize) -> Body {
        Body::Memory(vec![1; length].into())
    }

    #[test]
    fn lru_budget_idle_expiry_and_signed_keys() {
        let now = Instant::now();
        let mut cache = Cache::new(10);
        cache.insert("signed?a=1".into(), body(4), now + IDLE * 2, now);
        cache.insert(
            "signed?a=2".into(),
            body(4),
            now + IDLE * 2,
            now + Duration::from_secs(1),
        );
        assert!(
            cache
                .get("signed?a=1", now + Duration::from_secs(2))
                .is_some()
        );
        cache.insert(
            "third".into(),
            body(4),
            now + IDLE * 2,
            now + Duration::from_secs(3),
        );
        assert!(
            cache
                .get("signed?a=2", now + Duration::from_secs(3))
                .is_none()
        );
        assert_eq!(cache.bytes, 8);
        assert!(
            cache
                .get("signed?a=1", now + IDLE + Duration::from_secs(3))
                .is_none()
        );
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn response_policy_never_extends_server_freshness() {
        for value in [
            "no-store",
            "public, no-cache",
            "max-age=0",
            "max-age=bad",
            "max-age=10, no-store",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::CACHE_CONTROL, value.parse().unwrap());
            assert!(expiration(&headers).is_none(), "{value}");
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CACHE_CONTROL,
            "private, max-age=10".parse().unwrap(),
        );
        headers.insert(header::AGE, "9".parse().unwrap());
        assert!(
            expiration(&headers).unwrap().duration_since(Instant::now()) <= Duration::from_secs(1)
        );
        headers.insert(header::AGE, "10".parse().unwrap());
        assert!(expiration(&headers).is_none());
    }

    #[test]
    fn entry_count_and_server_expiry_still_bound_frequently_used_media() {
        let now = Instant::now();
        let mut cache = Cache::new(1024);
        for i in 0..MAX_ENTRIES + 5 {
            cache.insert(i.to_string(), body(1), now + IDLE, now);
        }
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert_eq!(cache.bytes, MAX_ENTRIES as u64);
        cache.insert("short".into(), body(1), now + Duration::from_secs(5), now);
        assert!(cache.get("short", now + Duration::from_secs(4)).is_some());
        assert!(cache.get("short", now + Duration::from_secs(6)).is_none());
        cache.insert(
            "oversize".into(),
            body(1025),
            now + IDLE,
            now + Duration::from_secs(6),
        );
        assert!(
            !cache
                .entries
                .contains_key(&Key::Complete("oversize".into()))
        );
    }

    #[test]
    fn retry_invalidates_full_and_partial_bodies_only_for_the_exact_resource() {
        let now = Instant::now();
        let mut cache = Cache::new(1024);
        cache.insert("url?one".into(), body(4), now + IDLE, now);
        cache.insert_key(Key::Range("url?one".into(), 10), body(4), now + IDLE, now);
        cache.insert("url?two".into(), body(4), now + IDLE, now);
        cache.invalidate("url?one");
        assert_eq!(cache.bytes, 4);
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.get("url?two", now).is_some());
    }

    #[test]
    fn readers_have_independent_positions_and_active_leases_survive_eviction() {
        use std::io::Write;
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"abcdef").unwrap();
        let disk = Body::Disk(Arc::new(Mutex::new(file)), 6);
        let now = Instant::now();
        let mut cache = Cache::new(6);
        cache.insert("one".into(), disk, now + IDLE, now);
        let a = cache.get("one", now).unwrap();
        let b = cache.get("one", now).unwrap();
        cache.insert("two".into(), body(6), now + IDLE, now);
        assert!(cache.get("one", now).is_none());
        let mut out = [0; 2];
        a.read_at(4, &mut out).unwrap();
        assert_eq!(&out, b"ef");
        b.read_at(0, &mut out).unwrap();
        assert_eq!(&out, b"ab");
    }
}
